use alloc::format;
use alloc::vec;
use core::sync::atomic::{AtomicU64, Ordering};

use limine::file::{File, LIMINE_MEDIA_TYPE_OPTICAL};
use nexos_storage::{
    BIOS_BOOT_TYPE_GUID, BlockDevice, ESP_TYPE_GUID, Fat32, Guid, InstallerGuids, InstallerLayout,
    NEXFS_TYPE_GUID, PartitionDevice, StorageError, crc32, format_fat32, parse_gpt_header,
    parse_mbr, read_partition_table, verify_guided_installer_gpt, write_guided_installer_gpt,
};

use crate::storage::StorageDevice;

const CONFIGURATION: &[u8] = b"timeout: 0\n\n/NexOS\n    protocol: limine\n    path: boot():/BOOT/NEXOS.ELF\n    module_path: boot():/BOOT/NEXSH.ELF\n    module_string: nexos-shell\n";

#[derive(Clone, Copy)]
pub enum BootSource {
    Optical,
    Gpt(Guid),
    Mbr(u32),
    Unknown,
}

#[derive(Clone, Copy)]
pub struct InstallPayload {
    pub kernel: &'static [u8],
    pub boot_x64: Option<&'static [u8]>,
    pub limine_hdd: Option<&'static [u8]>,
    pub limine_bios: Option<&'static [u8]>,
    pub shell: Option<&'static [u8]>,
    pub source: BootSource,
}

impl InstallPayload {
    #[must_use]
    pub fn from_bootloader(
        executable: Option<&'static File>,
        modules: &'static [&'static File],
    ) -> Self {
        let kernel = executable.map_or(&[][..], File::data);
        let source = executable.map_or(BootSource::Unknown, boot_source);
        let boot_x64 = modules
            .iter()
            .find(|module| {
                module.cmdline() == "nexos-bootx64" || module.path().ends_with("BOOTX64.EFI")
            })
            .map(|module| module.data());
        let limine_hdd = modules
            .iter()
            .find(|module| {
                module.cmdline() == "nexos-limine-hdd"
                    || module.path().ends_with("limine-bios-hdd.bin")
            })
            .map(|module| module.data());
        let limine_bios = modules
            .iter()
            .find(|module| {
                module.cmdline() == "nexos-limine-bios"
                    || module.path().ends_with("limine-bios.sys")
            })
            .map(|module| module.data());
        let shell = modules
            .iter()
            .find(|module| module.cmdline() == "nexos-shell" || module.path().ends_with("nexsh"))
            .map(|module| module.data());
        Self {
            kernel,
            boot_x64,
            limine_hdd,
            limine_bios,
            shell,
            source,
        }
    }

    #[must_use]
    pub const fn ready(self) -> bool {
        !self.kernel.is_empty()
            && self.boot_x64.is_some()
            && self.limine_hdd.is_some()
            && self.limine_bios.is_some()
            && self.shell.is_some()
    }
}

#[derive(Clone, Copy)]
pub struct InstallProgress {
    pub step: u8,
    pub total: u8,
    pub label: &'static str,
}

impl InstallProgress {
    const TOTAL: u8 = 9;

    const fn new(step: u8, label: &'static str) -> Self {
        Self {
            step,
            total: Self::TOTAL,
            label,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InstallerError {
    MissingPayload,
    UnknownBootSource,
    BootDevice,
    UnsupportedDisk,
    Storage(StorageError),
    Filesystem(nexfs::FsError),
    Verification,
}

impl InstallerError {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingPayload => "installer payload is missing",
            Self::UnknownBootSource => "live boot source could not be identified",
            Self::BootDevice => "target is the live boot device",
            Self::UnsupportedDisk => "target disk geometry is unsupported",
            Self::Storage(error) => storage_error_message(error),
            Self::Filesystem(nexfs::FsError::Storage(error)) => storage_error_message(error),
            Self::Filesystem(nexfs::FsError::TooSmall) => "filesystem target is too small",
            Self::Filesystem(nexfs::FsError::UnsupportedBlockSize) => {
                "filesystem block size is unsupported"
            }
            Self::Filesystem(nexfs::FsError::InvalidMagic) => "filesystem magic is invalid",
            Self::Filesystem(nexfs::FsError::UnsupportedVersion) => {
                "filesystem version is unsupported"
            }
            Self::Filesystem(nexfs::FsError::InvalidLayout) => "filesystem layout is invalid",
            Self::Filesystem(nexfs::FsError::ChecksumMismatch) => {
                "filesystem checksum does not match"
            }
            Self::Filesystem(nexfs::FsError::Dirty) => "filesystem is marked dirty",
            Self::Filesystem(nexfs::FsError::NotFound) => "filesystem object was not found",
            Self::Filesystem(nexfs::FsError::AlreadyExists) => "filesystem object already exists",
            Self::Filesystem(nexfs::FsError::NotDirectory) => {
                "filesystem object is not a directory"
            }
            Self::Filesystem(nexfs::FsError::IsDirectory) => "filesystem object is a directory",
            Self::Filesystem(nexfs::FsError::DirectoryNotEmpty) => "directory is not empty",
            Self::Filesystem(nexfs::FsError::InvalidName) => "filesystem name is invalid",
            Self::Filesystem(nexfs::FsError::InvalidPath) => "filesystem path is invalid",
            Self::Filesystem(nexfs::FsError::NoSpace) => "filesystem is out of space",
            Self::Filesystem(nexfs::FsError::FileTooLarge) => "filesystem file is too large",
            Self::Filesystem(nexfs::FsError::TooLarge) => "filesystem value is too large",
            Self::Filesystem(nexfs::FsError::CorruptInode) => "filesystem inode is corrupt",
            Self::Filesystem(nexfs::FsError::CorruptDirectory) => "filesystem directory is corrupt",
            Self::Filesystem(nexfs::FsError::Busy) => "filesystem is busy",
            Self::Verification => "installed data did not pass read-back verification",
        }
    }
}

impl From<StorageError> for InstallerError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

const fn storage_error_message(error: StorageError) -> &'static str {
    match error {
        StorageError::OutOfBounds => "storage request is outside the device",
        StorageError::InvalidBuffer => "storage buffer is invalid",
        StorageError::ReadOnly => "storage device is read-only",
        StorageError::InvalidMbr => "MBR is invalid",
        StorageError::InvalidGpt => "GPT is invalid",
        StorageError::InvalidPartition => "partition layout is invalid",
        StorageError::ChecksumMismatch => "storage checksum does not match",
        StorageError::UnsupportedSectorSize => "sector size is unsupported",
        StorageError::UnsupportedFilesystem => "filesystem is unsupported",
        StorageError::CorruptFilesystem => "filesystem is corrupt",
        StorageError::NotFound => "storage object was not found",
        StorageError::AlreadyExists => "storage object already exists",
        StorageError::InvalidName => "storage object name is invalid",
        StorageError::NoSpace => "storage device is out of space",
        StorageError::TooLarge => "storage value is too large",
        StorageError::Timeout => "storage request timed out",
        StorageError::Busy => "storage device is busy",
        StorageError::Device => "storage device reported an error",
    }
}

impl From<nexfs::FsError> for InstallerError {
    fn from(value: nexfs::FsError) -> Self {
        Self::Filesystem(value)
    }
}

#[derive(Clone, Copy)]
pub struct InstallReport {
    pub layout: InstallerLayout,
    pub root_uuid: [u8; 16],
    pub kernel_bytes: usize,
    pub kernel_crc32: u32,
}

pub fn install(
    device: &mut StorageDevice,
    payload: InstallPayload,
    mut progress: impl FnMut(InstallProgress),
) -> Result<InstallReport, InstallerError> {
    progress(InstallProgress::new(1, "validating target and payload"));
    let boot_x64 = payload.boot_x64.ok_or(InstallerError::MissingPayload)?;
    let limine_hdd = payload.limine_hdd.ok_or(InstallerError::MissingPayload)?;
    let limine_bios = payload.limine_bios.ok_or(InstallerError::MissingPayload)?;
    let shell = payload.shell.ok_or(InstallerError::MissingPayload)?;
    if payload.kernel.is_empty() {
        return Err(InstallerError::MissingPayload);
    }
    if matches!(payload.source, BootSource::Unknown) {
        return Err(InstallerError::UnknownBootSource);
    }
    if is_boot_device(device, payload.source)? {
        return Err(InstallerError::BootDevice);
    }
    let sector_size = device.sector_size();
    if !(512..=4096).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || device
            .sector_count()
            .checked_mul(u64::from(sector_size))
            .is_none_or(|bytes| bytes < 128 * 1024 * 1024)
    {
        return Err(InstallerError::UnsupportedDisk);
    }

    let seed = u64::from(crc32(payload.kernel))
        ^ device.sector_count().rotate_left(17)
        ^ crate::interrupts::ticks().rotate_left(39);
    let guids = InstallerGuids {
        disk: generate_guid(seed),
        bios: generate_guid(seed ^ 1),
        esp: generate_guid(seed ^ 2),
        root: generate_guid(seed ^ 3),
    };
    let root_uuid = generate_guid(seed ^ 4).0;
    progress(InstallProgress::new(2, "creating protective MBR and GPT"));
    let layout = write_guided_installer_gpt(device, guids)?;

    {
        progress(InstallProgress::new(3, "formatting FAT32 EFI partition"));
        let mut esp = PartitionDevice::new(device, layout.esp.first_lba, layout.esp.sector_count)?;
        let volume_id = u32::from_le_bytes(root_uuid[0..4].try_into().unwrap());
        format_fat32(&mut esp, volume_id)?;
        let mut filesystem = Fat32::mount(esp)?;
        progress(InstallProgress::new(4, "copying kernel and boot files"));
        filesystem.create_dir("/EFI")?;
        filesystem.create_dir("/EFI/BOOT")?;
        filesystem.create_dir("/BOOT")?;
        filesystem.write_file("/EFI/BOOT/BOOTX64.EFI", boot_x64)?;
        filesystem.write_file("/BOOT/NEXOS.ELF", payload.kernel)?;
        filesystem.write_file("/BOOT/NEXSH.ELF", shell)?;
        filesystem.write_file_lfn("/EFI/BOOT/limine.conf", "LIMINE~1.CON", CONFIGURATION)?;
        filesystem.write_file_lfn("/limine.conf", "LIMINE~1.CON", CONFIGURATION)?;
        filesystem.write_file_lfn("/limine-bios.sys", "LIMINE~1.SYS", limine_bios)?;
        let _ = filesystem.into_inner()?;
    }

    if sector_size == 512 {
        progress(InstallProgress::new(
            5,
            "installing legacy BIOS boot stages",
        ));
        install_bios_stages(device, &layout, limine_hdd)?;
    } else {
        progress(InstallProgress::new(
            5,
            "4Kn media: UEFI boot path selected",
        ));
    }

    {
        progress(InstallProgress::new(6, "formatting NexFS root partition"));
        let mut root =
            PartitionDevice::new(device, layout.root.first_lba, layout.root.sector_count)?;
        nexfs::format(&mut root, root_uuid)?;
        let mut filesystem = nexfs::NexFs::mount(&mut root)?;
        for directory in ["/system", "/etc", "/bin", "/var", "/home"] {
            filesystem.create_dir(directory)?;
        }
        write_nexfs_file(&mut filesystem, "/bin/nexsh", shell)?;
        write_nexfs_file(
            &mut filesystem,
            "/etc/nexos-release",
            b"NAME=NexOS\nVERSION=0.15.0-dev\nARCH=x86_64\n",
        )?;
        write_nexfs_file(
            &mut filesystem,
            "/etc/fstab",
            b"root / nexfs rw 0 1\nesp /boot fat32 rw 0 2\n",
        )?;
        let manifest = format!(
            "format=2\nversion=0.15.0-dev\nkernel_bytes={}\nkernel_crc32={:08x}\nshell_bytes={}\nshell_crc32={:08x}\n",
            payload.kernel.len(),
            crc32(payload.kernel),
            shell.len(),
            crc32(shell)
        );
        write_nexfs_file(
            &mut filesystem,
            "/system/install-manifest",
            manifest.as_bytes(),
        )?;
        filesystem.unmount()?;
    }
    progress(InstallProgress::new(7, "flushing storage caches"));
    device.flush()?;
    progress(InstallProgress::new(
        8,
        "verifying installed bytes and metadata",
    ));
    verify_installation(device, payload, &layout, root_uuid)?;
    progress(InstallProgress::new(9, "installation complete"));
    Ok(InstallReport {
        layout,
        root_uuid,
        kernel_bytes: payload.kernel.len(),
        kernel_crc32: crc32(payload.kernel),
    })
}

pub fn verify_installation(
    device: &mut StorageDevice,
    payload: InstallPayload,
    layout: &InstallerLayout,
    expected_root_uuid: [u8; 16],
) -> Result<(), InstallerError> {
    verify_guided_installer_gpt(device, layout)?;
    {
        let esp = PartitionDevice::new(device, layout.esp.first_lba, layout.esp.sector_count)?;
        let mut filesystem = Fat32::mount(esp)?;
        let installed_efi = filesystem.read_file("/EFI/BOOT/BOOTX64.EFI")?;
        if payload
            .boot_x64
            .is_none_or(|expected| installed_efi.as_slice() != expected)
        {
            return Err(InstallerError::Verification);
        }
        drop(installed_efi);
        let installed_kernel = filesystem.read_file("/BOOT/NEXOS.ELF")?;
        if installed_kernel.len() != payload.kernel.len()
            || crc32(&installed_kernel) != crc32(payload.kernel)
        {
            return Err(InstallerError::Verification);
        }
        drop(installed_kernel);
        let installed_shell = filesystem.read_file("/BOOT/NEXSH.ELF")?;
        if payload
            .shell
            .is_none_or(|expected| installed_shell.as_slice() != expected)
        {
            return Err(InstallerError::Verification);
        }
        drop(installed_shell);
        let configuration = filesystem.read_file("/EFI/BOOT/LIMINE~1.CON")?;
        if configuration != CONFIGURATION {
            return Err(InstallerError::Verification);
        }
        let root_configuration = filesystem.read_file("/LIMINE~1.CON")?;
        if root_configuration != CONFIGURATION {
            return Err(InstallerError::Verification);
        }
        let installed_bios = filesystem.read_file("/LIMINE~1.SYS")?;
        if payload
            .limine_bios
            .is_none_or(|expected| installed_bios.as_slice() != expected)
        {
            return Err(InstallerError::Verification);
        }
        let _ = filesystem.into_inner()?;
    }
    if device.sector_size() == 512 {
        verify_bios_stages(device, layout, payload.limine_hdd)?;
    }
    {
        let mut root =
            PartitionDevice::new(device, layout.root.first_lba, layout.root.sector_count)?;
        let superblock = nexfs::inspect_superblock(&mut root)?;
        if superblock.uuid != expected_root_uuid {
            return Err(InstallerError::Verification);
        }
        let mut filesystem = nexfs::NexFs::mount(&mut root)?;
        for path in [
            "/bin/nexsh",
            "/etc/nexos-release",
            "/etc/fstab",
            "/system/install-manifest",
        ] {
            let stat = filesystem.stat(path)?;
            if stat.kind != nexfs::FileType::Regular || stat.size == 0 {
                return Err(InstallerError::Verification);
            }
        }
        let shell_stat = filesystem.stat("/bin/nexsh")?;
        let shell_size =
            usize::try_from(shell_stat.size).map_err(|_| InstallerError::Verification)?;
        let mut shell = vec![0_u8; shell_size];
        if filesystem.read_file("/bin/nexsh", 0, &mut shell)? != shell.len()
            || payload
                .shell
                .is_none_or(|expected| crc32(&shell) != crc32(expected))
            || nexos_runtime::elf::ElfImage::parse(&shell).is_err()
        {
            return Err(InstallerError::Verification);
        }
        filesystem.unmount()?;
    }
    Ok(())
}

fn write_nexfs_file<D: BlockDevice>(
    filesystem: &mut nexfs::NexFs<'_, D>,
    path: &str,
    contents: &[u8],
) -> Result<(), InstallerError> {
    filesystem.create_file(path)?;
    filesystem.write_file(path, 0, contents)?;
    Ok(())
}

fn install_bios_stages(
    device: &mut StorageDevice,
    layout: &InstallerLayout,
    image: &[u8],
) -> Result<(), InstallerError> {
    if image.len() <= 512 {
        return Err(InstallerError::MissingPayload);
    }
    let stage2_bytes = image.len() - 512;
    let stage2_capacity = layout
        .bios
        .sector_count
        .checked_mul(512)
        .ok_or(InstallerError::UnsupportedDisk)?;
    if u64::try_from(stage2_bytes).map_err(|_| InstallerError::UnsupportedDisk)? > stage2_capacity {
        return Err(InstallerError::UnsupportedDisk);
    }

    let mut original = [0_u8; 512];
    device.read_sectors(0, &mut original)?;
    let mut boot_sector = [0_u8; 512];
    boot_sector.copy_from_slice(&image[..512]);
    boot_sector[218..224].copy_from_slice(&original[218..224]);
    boot_sector[440..510].copy_from_slice(&original[440..510]);
    let stage2_offset = layout
        .bios
        .first_lba
        .checked_mul(512)
        .ok_or(InstallerError::UnsupportedDisk)?;
    boot_sector[0x1a4..0x1ac].copy_from_slice(&stage2_offset.to_le_bytes());
    device.write_sectors(0, &boot_sector)?;

    let padded_length = stage2_bytes
        .div_ceil(512)
        .checked_mul(512)
        .ok_or(InstallerError::UnsupportedDisk)?;
    let mut stage2 = vec![0_u8; padded_length];
    stage2[..stage2_bytes].copy_from_slice(&image[512..]);
    let mut bios_partition =
        PartitionDevice::new(device, layout.bios.first_lba, layout.bios.sector_count)?;
    bios_partition.write_sectors(0, &stage2)?;
    bios_partition.flush()?;
    Ok(())
}

fn verify_bios_stages(
    device: &mut StorageDevice,
    layout: &InstallerLayout,
    image: Option<&[u8]>,
) -> Result<(), InstallerError> {
    let image = image.ok_or(InstallerError::MissingPayload)?;
    if image.len() <= 512 {
        return Err(InstallerError::MissingPayload);
    }
    let mut boot_sector = [0_u8; 512];
    device.read_sectors(0, &mut boot_sector)?;
    let stage2_offset = layout
        .bios
        .first_lba
        .checked_mul(512)
        .ok_or(InstallerError::UnsupportedDisk)?;
    if boot_sector[..218] != image[..218]
        || boot_sector[224..0x1a4] != image[224..0x1a4]
        || boot_sector[0x1a4..0x1ac] != stage2_offset.to_le_bytes()
        || boot_sector[0x1ac..440] != image[0x1ac..440]
        || boot_sector[510..512] != image[510..512]
    {
        return Err(InstallerError::Verification);
    }

    let stage2_bytes = image.len() - 512;
    let padded_length = stage2_bytes
        .div_ceil(512)
        .checked_mul(512)
        .ok_or(InstallerError::UnsupportedDisk)?;
    let mut installed = vec![0_u8; padded_length];
    let mut bios_partition =
        PartitionDevice::new(device, layout.bios.first_lba, layout.bios.sector_count)?;
    bios_partition.read_sectors(0, &mut installed)?;
    if installed[..stage2_bytes] != image[512..] {
        return Err(InstallerError::Verification);
    }
    Ok(())
}

pub fn verify_existing(
    device: &mut StorageDevice,
    payload: InstallPayload,
) -> Result<InstallReport, InstallerError> {
    if !payload.ready() {
        return Err(InstallerError::MissingPayload);
    }
    let table = read_partition_table(device)?;
    let disk_guid = table.disk_guid.ok_or(InstallerError::Verification)?;
    let find = |type_guid| {
        table
            .partitions
            .iter()
            .find(|partition| partition.type_guid == Some(type_guid))
            .map(|partition| nexos_storage::PartitionSpan {
                first_lba: partition.first_lba,
                sector_count: partition.sector_count,
            })
            .ok_or(InstallerError::Verification)
    };
    let layout = InstallerLayout {
        disk_guid,
        bios: find(BIOS_BOOT_TYPE_GUID)?,
        esp: find(ESP_TYPE_GUID)?,
        root: find(NEXFS_TYPE_GUID)?,
    };
    let root_uuid = {
        let mut root =
            PartitionDevice::new(device, layout.root.first_lba, layout.root.sector_count)?;
        nexfs::inspect_superblock(&mut root)?.uuid
    };
    verify_installation(device, payload, &layout, root_uuid)?;
    Ok(InstallReport {
        layout,
        root_uuid,
        kernel_bytes: payload.kernel.len(),
        kernel_crc32: crc32(payload.kernel),
    })
}

pub fn is_boot_device(
    device: &mut StorageDevice,
    source: BootSource,
) -> Result<bool, InstallerError> {
    match source {
        BootSource::Optical => Ok(false),
        BootSource::Unknown => Err(InstallerError::UnknownBootSource),
        BootSource::Mbr(expected) => {
            let sector_size = usize::try_from(device.sector_size())
                .map_err(|_| InstallerError::UnsupportedDisk)?;
            let mut sector = vec![0_u8; sector_size];
            device.read_sectors(0, &mut sector)?;
            let identifier = u32::from_le_bytes(sector[440..444].try_into().unwrap());
            Ok(identifier != 0 && identifier == expected)
        }
        BootSource::Gpt(expected) => {
            let sector_size = usize::try_from(device.sector_size())
                .map_err(|_| InstallerError::UnsupportedDisk)?;
            let mut sector = vec![0_u8; sector_size];
            device.read_sectors(0, &mut sector)?;
            let mbr = match parse_mbr(&sector) {
                Ok(mbr) => mbr,
                Err(_) => return Ok(false),
            };
            if !mbr
                .iter()
                .flatten()
                .any(|partition| partition.partition_type == 0xee)
            {
                return Ok(false);
            }
            device.read_sectors(1, &mut sector)?;
            Ok(parse_gpt_header(&sector).is_ok_and(|header| header.disk_guid == expected))
        }
    }
}

fn boot_source(file: &File) -> BootSource {
    if file.media_type == LIMINE_MEDIA_TYPE_OPTICAL {
        return BootSource::Optical;
    }
    if !file.gpt_disk_uuid.is_null() {
        return BootSource::Gpt(limine_guid(file.gpt_disk_uuid));
    }
    if file.mbr_disk_id != 0 {
        return BootSource::Mbr(file.mbr_disk_id);
    }
    BootSource::Unknown
}

fn limine_guid(value: limine::uuid::Uuid) -> Guid {
    let mut bytes = [0_u8; 16];
    bytes[0..4].copy_from_slice(&value.a.to_le_bytes());
    bytes[4..6].copy_from_slice(&value.b.to_le_bytes());
    bytes[6..8].copy_from_slice(&value.c.to_le_bytes());
    bytes[8..].copy_from_slice(&value.d);
    Guid(bytes)
}

fn generate_guid(seed: u64) -> Guid {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let first = mix(seed ^ counter.rotate_left(23));
    let second = mix(seed ^ counter ^ 0xd6e8_feb8_6659_fd93);
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&first.to_le_bytes());
    bytes[8..].copy_from_slice(&second.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Guid(bytes)
}

const fn mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
