use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use nexfs::{FileType, NexFs};
use nexos_storage::MemoryBlockDevice;

use crate::VERSION;
use crate::disk::{
    DiskReport, FilesystemKind, GuidedMode, MIB, PartitionReport, PartitionRole, commit_prepared,
    create_blank_bytes, format_partition, generate_guid, guided_layout, hex_guid, inspect_bytes,
    partition_range, prepared_path, read_image, require_confirmation, validate_image_path,
    write_layout, write_prepared,
};

const MIN_ESP_BYTES: u64 = 32 * MIB as u64;
const MIN_ROOT_BYTES: u64 = 8 * MIB as u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallMode {
    Guided(GuidedMode),
    Manual {
        esp_partition: u32,
        root_partition: u32,
        bios: bool,
    },
}

impl InstallMode {
    #[must_use]
    pub const fn has_bios(self) -> bool {
        match self {
            Self::Guided(mode) => mode.has_bios(),
            Self::Manual { bios, .. } => bios,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Guided(GuidedMode::Combined) => "guided-combined",
            Self::Guided(GuidedMode::Uefi) => "guided-uefi",
            Self::Guided(GuidedMode::Bios) => "guided-bios",
            Self::Manual { .. } => "manual",
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstallRequest {
    pub target: PathBuf,
    pub kernel: PathBuf,
    pub limine_directory: PathBuf,
    pub size_mib: usize,
    pub mode: InstallMode,
    pub yes: bool,
    pub confirmation: String,
    pub install_bios_stage: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReport {
    pub target: PathBuf,
    pub mode: &'static str,
    pub esp_partition: u32,
    pub root_partition: u32,
    pub root_uuid: [u8; 16],
    pub kernel_bytes: usize,
    pub verified_files: u8,
    pub bios_stage_installed: bool,
}

pub fn install(request: &InstallRequest) -> Result<InstallReport, String> {
    validate_request(request)?;
    require_confirmation(&request.target, request.yes, &request.confirmation)?;
    let kernel = fs::read(&request.kernel)
        .map_err(|error| format!("cannot read kernel {}: {error}", request.kernel.display()))?;
    let boot_x64 = read_required(&request.limine_directory.join("BOOTX64.EFI"))?;
    let bios_sys = read_required(&request.limine_directory.join("limine-bios.sys"))?;

    let (mut image, disk_report, esp_index, root_index) = match request.mode {
        InstallMode::Guided(mode) => {
            let mut image = create_blank_bytes(request.size_mib)?;
            let layout = guided_layout(request.size_mib, mode)?;
            write_layout(&mut image, &layout)?;
            let report = inspect_bytes(&image)?;
            let esp = find_role(&report, PartitionRole::EfiSystem)?.index;
            let root = find_role(&report, PartitionRole::NexFs)?.index;
            (image, report, esp, root)
        }
        InstallMode::Manual {
            esp_partition,
            root_partition,
            ..
        } => {
            if esp_partition == root_partition {
                return Err("manual ESP and root partitions must be different".into());
            }
            let image = read_image(&request.target)?;
            let report = inspect_bytes(&image)?;
            let esp = find_index(&report, esp_partition)?;
            let root = find_index(&report, root_partition)?;
            validate_selected_partitions(esp, root)?;
            (image, report, esp_partition, root_partition)
        }
    };
    let esp = find_index(&disk_report, esp_index)?.clone();
    let root = find_index(&disk_report, root_index)?.clone();
    validate_selected_partitions(&esp, &root)?;

    let root_uuid = generate_guid().0;
    format_partition(&mut image, &esp, FilesystemKind::Fat32, generate_guid().0)?;
    populate_esp(&mut image, &esp, &kernel, &boot_x64, &bios_sys)?;
    format_partition(&mut image, &root, FilesystemKind::NexFs, root_uuid)?;
    populate_root(
        &mut image,
        &root,
        &kernel,
        root_uuid,
        esp_index,
        root_index,
        request.mode,
    )?;

    let temporary = prepared_path(&request.target)?;
    write_prepared(&temporary, &image)?;
    let preparation = (|| {
        if request.mode.has_bios() && request.install_bios_stage {
            install_limine_bios(&temporary, &request.limine_directory)?;
        }
        verify_selected_partitions(&temporary, esp_index, root_index)
    })();
    let verified = match preparation {
        Ok(report) => report,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "installation preparation failed; target was not changed: {error}"
            ));
        }
    };
    if verified.esp_partition != esp_index || verified.root_partition != root_index {
        let _ = fs::remove_file(&temporary);
        return Err("installed partition selection changed during verification".into());
    }
    commit_prepared(&request.target, &temporary)?;
    let final_verification = verify_selected_partitions(&request.target, esp_index, root_index)?;
    if final_verification.root_uuid != root_uuid {
        return Err("post-commit root UUID verification failed".into());
    }
    Ok(InstallReport {
        target: request.target.clone(),
        mode: request.mode.name(),
        esp_partition: esp_index,
        root_partition: root_index,
        root_uuid,
        kernel_bytes: kernel.len(),
        verified_files: final_verification.verified_files,
        bios_stage_installed: request.mode.has_bios() && request.install_bios_stage,
    })
}

pub fn verify_installed_image(path: &Path) -> Result<InstallReport, String> {
    let image = read_image(path)?;
    let report = inspect_bytes(&image)?;
    let esp = find_role(&report, PartitionRole::EfiSystem)?;
    let root = find_role(&report, PartitionRole::NexFs)?;
    verify_selected(&image, path, esp, root)
}

fn verify_selected_partitions(
    path: &Path,
    esp_index: u32,
    root_index: u32,
) -> Result<InstallReport, String> {
    let image = read_image(path)?;
    let report = inspect_bytes(&image)?;
    let esp = find_index(&report, esp_index)?;
    let root = find_index(&report, root_index)?;
    verify_selected(&image, path, esp, root)
}

fn verify_selected(
    image: &[u8],
    path: &Path,
    esp: &PartitionReport,
    root: &PartitionReport,
) -> Result<InstallReport, String> {
    let mut verified_files = verify_esp(image, esp)?;
    let (root_uuid, root_files) = verify_root(image, root)?;
    verified_files = verified_files.saturating_add(root_files);
    Ok(InstallReport {
        target: path.to_path_buf(),
        mode: "verified",
        esp_partition: esp.index,
        root_partition: root.index,
        root_uuid,
        kernel_bytes: 0,
        verified_files,
        bios_stage_installed: false,
    })
}

fn validate_request(request: &InstallRequest) -> Result<(), String> {
    validate_image_path(&request.target)?;
    if request.target == request.kernel || request.target.starts_with(&request.limine_directory) {
        return Err("target image must not be an installer source file".into());
    }
    let kernel_metadata = fs::symlink_metadata(&request.kernel)
        .map_err(|error| format!("cannot inspect kernel: {error}"))?;
    if !kernel_metadata.file_type().is_file() || kernel_metadata.file_type().is_symlink() {
        return Err("kernel must be a regular non-symlink file".into());
    }
    let limine_metadata = fs::metadata(&request.limine_directory)
        .map_err(|error| format!("cannot inspect Limine directory: {error}"))?;
    if !limine_metadata.is_dir() {
        return Err("Limine path must be a directory".into());
    }
    if matches!(request.mode, InstallMode::Manual { .. }) && !request.target.exists() {
        return Err("manual installation needs an existing partitioned image".into());
    }
    Ok(())
}

fn validate_selected_partitions(
    esp: &PartitionReport,
    root: &PartitionReport,
) -> Result<(), String> {
    if esp.sector_count.saturating_mul(512) < MIN_ESP_BYTES {
        return Err("selected EFI/boot partition is smaller than 32 MiB".into());
    }
    if root.sector_count.saturating_mul(512) < MIN_ROOT_BYTES {
        return Err("selected root partition is smaller than 8 MiB".into());
    }
    let esp_end = esp.first_lba.saturating_add(esp.sector_count);
    let root_end = root.first_lba.saturating_add(root.sector_count);
    if esp.first_lba < root_end && root.first_lba < esp_end {
        return Err("selected EFI/boot and root partitions overlap".into());
    }
    Ok(())
}

fn populate_esp(
    image: &mut [u8],
    partition: &PartitionReport,
    kernel: &[u8],
    boot_x64: &[u8],
    bios_sys: &[u8],
) -> Result<(), String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let mut cursor = Cursor::new(&mut image[range]);
    cursor
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let filesystem = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
        .map_err(|error| format!("cannot mount new EFI filesystem: {error}"))?;
    {
        let root = filesystem.root_dir();
        root.create_dir("EFI")
            .map_err(|error| format!("cannot create EFI: {error}"))?;
        root.create_dir("EFI/BOOT")
            .map_err(|error| format!("cannot create EFI/BOOT: {error}"))?;
        root.create_dir("boot")
            .map_err(|error| format!("cannot create boot: {error}"))?;
        write_fat_file(&root, "EFI/BOOT/BOOTX64.EFI", boot_x64)?;
        write_fat_file(&root, "boot/limine-bios.sys", bios_sys)?;
        write_fat_file(&root, "boot/nexos-kernel", kernel)?;
        write_fat_file(
            &root,
            "limine.conf",
            b"timeout: 0\n\n/NexOS\n    protocol: limine\n    path: boot():/boot/nexos-kernel\n",
        )?;
    }
    filesystem
        .unmount()
        .map_err(|error| format!("cannot finalize EFI filesystem: {error}"))
}

fn populate_root(
    image: &mut [u8],
    partition: &PartitionReport,
    kernel: &[u8],
    root_uuid: [u8; 16],
    esp_index: u32,
    root_index: u32,
    mode: InstallMode,
) -> Result<(), String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let partition_bytes = &mut image[range];
    let mut disk = MemoryBlockDevice::from_bytes(partition_bytes.to_vec(), 512)
        .map_err(|error| format!("invalid NexFS root partition: {error:?}"))?;
    let mut filesystem =
        NexFs::mount(&mut disk).map_err(|error| format!("cannot mount NexFS root: {error:?}"))?;
    for directory in ["/system", "/etc", "/bin", "/var", "/home"] {
        filesystem
            .create_dir(directory)
            .map_err(|error| format!("cannot create {directory}: {error:?}"))?;
    }
    write_nexfs_file(
        &mut filesystem,
        "/system/kernel-location",
        b"esp:/boot/nexos-kernel\n",
    )?;
    write_nexfs_file(
        &mut filesystem,
        "/etc/nexos-release",
        format!(
            "NAME=NexOS\nVERSION={VERSION}\nARCH=x86_64\nINSTALL_MODE={}\n",
            mode.name()
        )
        .as_bytes(),
    )?;
    write_nexfs_file(
        &mut filesystem,
        "/etc/fstab",
        format!(
            "UUID={} / nexfs rw 0 1\npartition:{esp_index} /boot fat32 rw 0 2\n",
            hex_guid(nexos_storage::Guid(root_uuid))
        )
        .as_bytes(),
    )?;
    write_nexfs_file(
        &mut filesystem,
        "/system/install-manifest",
        format!(
            "format=1\nversion={VERSION}\nesp_partition={esp_index}\nroot_partition={root_index}\nkernel_bytes={}\nkernel_crc32={:08x}\n",
            kernel.len(),
            nexos_storage::crc32(kernel)
        )
        .as_bytes(),
    )?;
    filesystem
        .unmount()
        .map_err(|error| format!("cannot finalize NexFS root: {error:?}"))?;
    partition_bytes.copy_from_slice(disk.bytes());
    Ok(())
}

fn verify_esp(image: &[u8], partition: &PartitionReport) -> Result<u8, String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let cursor = Cursor::new(image[range].to_vec());
    let filesystem = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
        .map_err(|error| format!("cannot verify EFI filesystem: {error}"))?;
    let root = filesystem.root_dir();
    let mut count = 0_u8;
    for path in [
        "EFI/BOOT/BOOTX64.EFI",
        "boot/limine-bios.sys",
        "boot/nexos-kernel",
        "limine.conf",
    ] {
        let mut file = root
            .open_file(path)
            .map_err(|error| format!("installed EFI file {path} is missing: {error}"))?;
        let mut first = [0_u8; 1];
        let bytes = file
            .read(&mut first)
            .map_err(|error| format!("cannot read installed EFI file {path}: {error}"))?;
        if bytes == 0 {
            return Err(format!("installed EFI file {path} is empty"));
        }
        count = count.saturating_add(1);
    }
    Ok(count)
}

fn verify_root(image: &[u8], partition: &PartitionReport) -> Result<([u8; 16], u8), String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let mut disk = MemoryBlockDevice::from_bytes(image[range].to_vec(), 512)
        .map_err(|error| format!("invalid installed root: {error:?}"))?;
    let checked = nexfs::check_detailed(&mut disk)
        .map_err(|error| format!("installed root check failed: {error:?}"))?;
    let root_uuid = checked.superblock.uuid;
    let mut filesystem = NexFs::mount(&mut disk)
        .map_err(|error| format!("cannot mount installed root: {error:?}"))?;
    let mut count = 0_u8;
    for path in [
        "/system/kernel-location",
        "/etc/nexos-release",
        "/etc/fstab",
        "/system/install-manifest",
    ] {
        let stat = filesystem
            .stat(path)
            .map_err(|error| format!("installed root file {path} is missing: {error:?}"))?;
        if stat.kind != FileType::Regular || stat.size == 0 {
            return Err(format!("installed root file {path} is invalid"));
        }
        count = count.saturating_add(1);
    }
    filesystem
        .unmount()
        .map_err(|error| format!("cannot finish root verification: {error:?}"))?;
    Ok((root_uuid, count))
}

fn install_limine_bios(image: &Path, limine_directory: &Path) -> Result<(), String> {
    let windows_tool = limine_directory
        .join("limine-tool-windows-x86")
        .join("limine.exe");
    let windows_fallback = limine_directory.join("limine.exe");
    let unix_tool = limine_directory.join("limine");
    let candidates = if cfg!(windows) {
        [windows_tool, windows_fallback, unix_tool]
    } else {
        [unix_tool, windows_fallback, windows_tool]
    };
    let tool = candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .ok_or("cannot find a Limine BIOS installation tool")?;
    let output = Command::new(tool)
        .arg("bios-install")
        .arg(image)
        .output()
        .map_err(|error| format!("cannot start Limine installer: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Limine BIOS installation failed: {}{}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(())
}

fn write_fat_file<T: fatfs::ReadWriteSeek>(
    root: &fatfs::Dir<'_, T>,
    path: &str,
    contents: &[u8],
) -> Result<(), String> {
    let mut file = root
        .create_file(path)
        .map_err(|error| format!("cannot create {path}: {error}"))?;
    file.truncate()
        .map_err(|error| format!("cannot truncate {path}: {error}"))?;
    file.write_all(contents)
        .map_err(|error| format!("cannot write {path}: {error}"))
}

fn write_nexfs_file(
    filesystem: &mut NexFs<'_, MemoryBlockDevice>,
    path: &str,
    contents: &[u8],
) -> Result<(), String> {
    filesystem
        .create_file(path)
        .map_err(|error| format!("cannot create {path}: {error:?}"))?;
    filesystem
        .write_file(path, 0, contents)
        .map_err(|error| format!("cannot write {path}: {error:?}"))?;
    Ok(())
}

fn find_role(report: &DiskReport, role: PartitionRole) -> Result<&PartitionReport, String> {
    report
        .partitions
        .iter()
        .find(|partition| partition.role == role)
        .ok_or_else(|| format!("disk has no {role:?} partition"))
}

fn find_index(report: &DiskReport, index: u32) -> Result<&PartitionReport, String> {
    report
        .partitions
        .iter()
        .find(|partition| partition.index == index)
        .ok_or_else(|| format!("partition {index} does not exist"))
}

fn read_required(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{GuidedMode, create_blank_bytes, guided_layout, inspect_image, write_layout};

    #[test]
    fn guided_install_is_verified_and_populated() {
        let directory = test_directory("guided");
        fs::create_dir_all(&directory).unwrap();
        let kernel = directory.join("nexos-kernel");
        let limine = directory.join("limine");
        fs::create_dir_all(&limine).unwrap();
        fs::write(&kernel, b"test-kernel-elf").unwrap();
        fs::write(limine.join("BOOTX64.EFI"), b"test-efi-loader").unwrap();
        fs::write(limine.join("limine-bios.sys"), b"test-bios-stage").unwrap();
        let target = directory.join("installed.img");
        let report = install(&InstallRequest {
            target: target.clone(),
            kernel,
            limine_directory: limine,
            size_mib: 128,
            mode: InstallMode::Guided(GuidedMode::Combined),
            yes: true,
            confirmation: target.to_string_lossy().into_owned(),
            install_bios_stage: false,
        })
        .unwrap();
        assert_eq!(report.verified_files, 8);
        assert_eq!(report.esp_partition, 2);
        assert_eq!(report.root_partition, 3);
        assert_eq!(inspect_image(&target).unwrap().partitions.len(), 3);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manual_install_preserves_unselected_partition() {
        let directory = test_directory("manual");
        fs::create_dir_all(&directory).unwrap();
        let kernel = directory.join("nexos-kernel");
        let limine = directory.join("limine");
        fs::create_dir_all(&limine).unwrap();
        fs::write(&kernel, b"test-kernel-elf").unwrap();
        fs::write(limine.join("BOOTX64.EFI"), b"test-efi-loader").unwrap();
        fs::write(limine.join("limine-bios.sys"), b"test-bios-stage").unwrap();
        let target = directory.join("manual.img");
        let mut image = create_blank_bytes(128).unwrap();
        let layout = guided_layout(128, GuidedMode::Combined).unwrap();
        write_layout(&mut image, &layout).unwrap();
        let bios = &layout.partitions[0];
        let preserved_range =
            partition_range(image.len(), bios.first_lba, bios.sector_count).unwrap();
        image[preserved_range.clone()].fill(0x5a);
        crate::disk::write_transactional(&target, &image).unwrap();

        install(&InstallRequest {
            target: target.clone(),
            kernel,
            limine_directory: limine,
            size_mib: 128,
            mode: InstallMode::Manual {
                esp_partition: 2,
                root_partition: 3,
                bios: false,
            },
            yes: true,
            confirmation: target.to_string_lossy().into_owned(),
            install_bios_stage: false,
        })
        .unwrap();
        let installed = fs::read(&target).unwrap();
        assert!(installed[preserved_range].iter().all(|byte| *byte == 0x5a));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn confirmation_failure_never_creates_target() {
        let directory = test_directory("confirm");
        fs::create_dir_all(&directory).unwrap();
        let kernel = directory.join("nexos-kernel");
        let limine = directory.join("limine");
        fs::create_dir_all(&limine).unwrap();
        fs::write(&kernel, b"kernel").unwrap();
        fs::write(limine.join("BOOTX64.EFI"), b"efi").unwrap();
        fs::write(limine.join("limine-bios.sys"), b"bios").unwrap();
        let target = directory.join("refused.img");
        assert!(
            install(&InstallRequest {
                target: target.clone(),
                kernel,
                limine_directory: limine,
                size_mib: 128,
                mode: InstallMode::Guided(GuidedMode::Combined),
                yes: true,
                confirmation: "wrong-target".into(),
                install_bios_stage: false,
            })
            .is_err()
        );
        assert!(!target.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory(name: &str) -> PathBuf {
        let unique = generate_guid();
        std::env::temp_dir().join(format!(
            "nexos-installer-{name}-{}",
            hex_guid(unique).replace('-', "")
        ))
    }
}
