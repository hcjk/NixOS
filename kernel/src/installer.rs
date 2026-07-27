use core::sync::atomic::{AtomicU64, Ordering};

use limine::file::{File, LIMINE_MEDIA_TYPE_OPTICAL};
use nexos_storage::{
    BIOS_BOOT_TYPE_GUID, BlockDevice, ESP_TYPE_GUID, Fat32, Guid, InstallerGuids, InstallerLayout,
    NEXFS_TYPE_GUID, PartitionDevice, StorageError, crc32, format_fat32, parse_gpt_header,
    parse_mbr, read_partition_table, verify_guided_installer_gpt, write_guided_installer_gpt,
};

use crate::storage::StorageDevice;

const CONFIGURATION: &[u8] =
    b"timeout: 0\n\n/NexOS\n    protocol: limine\n    path: boot():/BOOT/NEXOS.ELF\n";

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
        Self {
            kernel,
            boot_x64,
            source,
        }
    }

    #[must_use]
    pub const fn ready(self) -> bool {
        !self.kernel.is_empty() && self.boot_x64.is_some()
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
) -> Result<InstallReport, InstallerError> {
    let boot_x64 = payload.boot_x64.ok_or(InstallerError::MissingPayload)?;
    if payload.kernel.is_empty() {
        return Err(InstallerError::MissingPayload);
    }
    if matches!(payload.source, BootSource::Unknown) {
        return Err(InstallerError::UnknownBootSource);
    }
    if is_boot_device(device, payload.source)? {
        return Err(InstallerError::BootDevice);
    }
    if device.sector_size() != 512 || device.sector_count() < 262_144 {
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
    let layout = write_guided_installer_gpt(device, guids)?;

    {
        let mut esp = PartitionDevice::new(device, layout.esp.first_lba, layout.esp.sector_count)?;
        let volume_id = u32::from_le_bytes(root_uuid[0..4].try_into().unwrap());
        format_fat32(&mut esp, volume_id)?;
        let mut filesystem = Fat32::mount(esp)?;
        filesystem.create_dir("/EFI")?;
        filesystem.create_dir("/EFI/BOOT")?;
        filesystem.create_dir("/BOOT")?;
        filesystem.write_file("/EFI/BOOT/BOOTX64.EFI", boot_x64)?;
        filesystem.write_file("/BOOT/NEXOS.ELF", payload.kernel)?;
        filesystem.write_file_lfn("/EFI/BOOT/limine.conf", "LIMINE~1.CON", CONFIGURATION)?;
        let _ = filesystem.into_inner()?;
    }

    {
        let mut root =
            PartitionDevice::new(device, layout.root.first_lba, layout.root.sector_count)?;
        nexfs::format(&mut root, root_uuid)?;
    }
    device.flush()?;
    verify_installation(device, payload, &layout, root_uuid)?;
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
        let configuration = filesystem.read_file("/EFI/BOOT/LIMINE~1.CON")?;
        if configuration != CONFIGURATION {
            return Err(InstallerError::Verification);
        }
        let _ = filesystem.into_inner()?;
    }
    {
        let mut root =
            PartitionDevice::new(device, layout.root.first_lba, layout.root.sector_count)?;
        let superblock = nexfs::inspect_superblock(&mut root)?;
        if superblock.uuid != expected_root_uuid {
            return Err(InstallerError::Verification);
        }
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
            let mut sector = [0_u8; 512];
            device.read_sectors(0, &mut sector)?;
            let identifier = u32::from_le_bytes(sector[440..444].try_into().unwrap());
            Ok(identifier != 0 && identifier == expected)
        }
        BootSource::Gpt(expected) => {
            let mut sector = [0_u8; 512];
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
