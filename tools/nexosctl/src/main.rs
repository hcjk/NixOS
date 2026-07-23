use std::env;
use std::fs;
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use nexos_storage::MemoryBlockDevice;

const MIB: usize = 1024 * 1024;
const MIN_IMAGE_MIB: usize = 1;

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if let Err(message) = run(&arguments) {
        eprintln!("nexosctl: {message}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [command, path, size] if command == "create-image" => {
            let size_mib = size
                .parse::<usize>()
                .map_err(|_| "size must be an integer number of MiB")?;
            create_image(Path::new(path), size_mib)
        }
        [command, path] if command == "fs-info" => fs_info(Path::new(path)),
        [command, path] if command == "fs-check" => fs_check(Path::new(path)),
        [command, path, flag] if command == "mkfs" && flag == "--yes" => {
            format_existing(Path::new(path))
        }
        [command, output, kernel, limine] if command == "boot-image" => {
            build_boot_image(Path::new(output), Path::new(kernel), Path::new(limine), 128)
        }
        [command, output, kernel, limine, size] if command == "boot-image" => {
            let size_mib = size
                .parse::<usize>()
                .map_err(|_| "size must be an integer number of MiB")?;
            build_boot_image(
                Path::new(output),
                Path::new(kernel),
                Path::new(limine),
                size_mib,
            )
        }
        [] | [..] => Err(usage()),
    }
}

fn build_boot_image(
    output: &Path,
    kernel: &Path,
    limine_dir: &Path,
    size_mib: usize,
) -> Result<(), String> {
    const PARTITION_START_LBA: usize = 2048;
    const SECTOR_SIZE: usize = 512;

    validate_image_path(output)?;
    if output.exists() {
        return Err("refusing to overwrite an existing boot image".into());
    }
    if size_mib < 64 {
        return Err("boot image must be at least 64 MiB".into());
    }
    let kernel_bytes = fs::read(kernel)
        .map_err(|error| format!("cannot read kernel {}: {error}", kernel.display()))?;
    let boot_x64 = fs::read(limine_dir.join("BOOTX64.EFI"))
        .map_err(|error| format!("cannot read Limine BOOTX64.EFI: {error}"))?;
    let bios_sys = fs::read(limine_dir.join("limine-bios.sys"))
        .map_err(|error| format!("cannot read Limine BIOS stage: {error}"))?;

    let byte_count = size_mib
        .checked_mul(MIB)
        .ok_or("requested image is too large")?;
    let partition_offset = PARTITION_START_LBA * SECTOR_SIZE;
    if partition_offset >= byte_count {
        return Err("image is too small for the boot partition".into());
    }
    let partition_sectors = (byte_count - partition_offset) / SECTOR_SIZE;
    if partition_sectors > u32::MAX as usize {
        return Err("MBR boot images are limited to 2 TiB".into());
    }

    let mut image = vec![0_u8; byte_count];
    let first_lba =
        u32::try_from(PARTITION_START_LBA).map_err(|_| "partition offset is too large")?;
    let sector_count =
        u32::try_from(partition_sectors).map_err(|_| "partition is too large for MBR")?;
    write_boot_mbr(&mut image[..SECTOR_SIZE], first_lba, sector_count);
    {
        let partition = &mut image[partition_offset..];
        let mut cursor = Cursor::new(partition);
        fatfs::format_volume(
            &mut cursor,
            fatfs::FormatVolumeOptions::new()
                .fat_type(fatfs::FatType::Fat32)
                .volume_label(*b"NEXOS BOOT "),
        )
        .map_err(|error| format!("FAT32 format failed: {error}"))?;
        cursor
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot rewind FAT32 partition: {error}"))?;
        let filesystem = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
            .map_err(|error| format!("cannot mount new FAT32 partition: {error}"))?;
        {
            let root = filesystem.root_dir();
            root.create_dir("EFI")
                .map_err(|error| format!("cannot create EFI directory: {error}"))?;
            root.create_dir("EFI/BOOT")
                .map_err(|error| format!("cannot create EFI boot directory: {error}"))?;
            root.create_dir("boot")
                .map_err(|error| format!("cannot create boot directory: {error}"))?;
            write_fat_file(&root, "EFI/BOOT/BOOTX64.EFI", &boot_x64)?;
            write_fat_file(&root, "boot/limine-bios.sys", &bios_sys)?;
            write_fat_file(&root, "boot/nexos-kernel", &kernel_bytes)?;
            write_fat_file(
                &root,
                "limine.conf",
                b"timeout: 0\n\n/NexOS\n    protocol: limine\n    path: boot():/boot/nexos-kernel\n",
            )?;
        }
        filesystem
            .unmount()
            .map_err(|error| format!("cannot finalize FAT32 partition: {error}"))?;
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    write_new(output, &image)?;
    println!(
        "Created {size_mib} MiB BIOS/UEFI boot image at {}",
        output.display()
    );
    Ok(())
}

fn write_boot_mbr(sector: &mut [u8], first_lba: u32, sector_count: u32) {
    let entry = &mut sector[446..462];
    entry[0] = 0x80;
    entry[1..4].fill(0xff);
    entry[4] = 0xef;
    entry[5..8].fill(0xff);
    entry[8..12].copy_from_slice(&first_lba.to_le_bytes());
    entry[12..16].copy_from_slice(&sector_count.to_le_bytes());
    sector[510..512].copy_from_slice(&nexos_storage::MBR_SIGNATURE);
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

fn create_image(path: &Path, size_mib: usize) -> Result<(), String> {
    validate_image_path(path)?;
    if size_mib < MIN_IMAGE_MIB {
        return Err(format!("image must be at least {MIN_IMAGE_MIB} MiB"));
    }
    if path.exists() {
        return Err("refusing to overwrite an existing image".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let byte_count = size_mib
        .checked_mul(MIB)
        .ok_or("requested image is too large")?;
    let mut disk = MemoryBlockDevice::from_bytes(vec![0; byte_count], 512)
        .map_err(|error| format!("cannot create block device: {error:?}"))?;
    let superblock = nexfs::format(&mut disk, generate_uuid())
        .map_err(|error| format!("format failed: {error:?}"))?;
    write_new(path, &disk.into_bytes())?;
    println!(
        "Created {} MiB NexFS image at {} ({} blocks)",
        size_mib,
        path.display(),
        superblock.total_blocks
    );
    Ok(())
}

fn format_existing(path: &Path) -> Result<(), String> {
    validate_existing_regular_file(path)?;
    let bytes = fs::read(path).map_err(io_error)?;
    let mut disk = MemoryBlockDevice::from_bytes(bytes, 512)
        .map_err(|error| format!("invalid image: {error:?}"))?;
    let superblock = nexfs::format(&mut disk, generate_uuid())
        .map_err(|error| format!("format failed: {error:?}"))?;
    fs::write(path, disk.into_bytes()).map_err(io_error)?;
    println!(
        "Formatted {} as NexFS v{} ({} blocks)",
        path.display(),
        superblock.version,
        superblock.total_blocks
    );
    Ok(())
}

fn fs_info(path: &Path) -> Result<(), String> {
    let superblock = read_superblock(path)?;
    println!("filesystem: NexFS");
    println!("version: {}", superblock.version);
    println!("block size: {}", superblock.block_size);
    println!("blocks: {}", superblock.total_blocks);
    println!("data starts: {}", superblock.data_start_block);
    println!("clean: {}", superblock.is_clean());
    println!("uuid: {}", hex_uuid(superblock.uuid));
    Ok(())
}

fn fs_check(path: &Path) -> Result<(), String> {
    let superblock = read_superblock(path)?;
    println!(
        "{}: clean NexFS v{}, {} blocks",
        path.display(),
        superblock.version,
        superblock.total_blocks
    );
    Ok(())
}

fn read_superblock(path: &Path) -> Result<nexfs::Superblock, String> {
    validate_existing_regular_file(path)?;
    let bytes = fs::read(path).map_err(io_error)?;
    let mut disk = MemoryBlockDevice::from_bytes(bytes, 512)
        .map_err(|error| format!("invalid image: {error:?}"))?;
    nexfs::check(&mut disk).map_err(|error| format!("filesystem check failed: {error:?}"))
}

fn validate_image_path(path: &Path) -> Result<(), String> {
    let text = path.as_os_str().to_string_lossy();
    if text.starts_with(r"\\.\") || text.starts_with(r"\\?\GLOBALROOT") {
        return Err("raw Windows device paths are disabled in this milestone".into());
    }
    Ok(())
}

fn validate_existing_regular_file(path: &Path) -> Result<(), String> {
    validate_image_path(path)?;
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() {
        return Err("target must be an ordinary disk-image file".into());
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn generate_uuid() -> [u8; 16] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut uuid = nanos.to_le_bytes();
    uuid[6] = (uuid[6] & 0x0f) | 0x40;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
}

fn hex_uuid(uuid: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        uuid[0],
        uuid[1],
        uuid[2],
        uuid[3],
        uuid[4],
        uuid[5],
        uuid[6],
        uuid[7],
        uuid[8],
        uuid[9],
        uuid[10],
        uuid[11],
        uuid[12],
        uuid[13],
        uuid[14],
        uuid[15]
    )
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn usage() -> String {
    [
        "usage:",
        "  nexosctl create-image <path> <size-MiB>",
        "  nexosctl fs-info <path>",
        "  nexosctl fs-check <path>",
        "  nexosctl mkfs <existing-image> --yes",
        "  nexosctl boot-image <output> <kernel-elf> <limine-dir> [size-MiB]",
        "",
        "Physical disks are deliberately disabled until installer transaction",
        "logging, mounted-device detection, and recovery are implemented.",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_windows_raw_devices() {
        assert!(validate_image_path(Path::new(r"\\.\PhysicalDrive0")).is_err());
        assert!(validate_image_path(Path::new("disk.img")).is_ok());
    }

    #[test]
    fn generated_uuid_has_version_and_variant() {
        let uuid = generate_uuid();
        assert_eq!(uuid[6] >> 4, 4);
        assert_eq!(uuid[8] >> 6, 2);
    }
}
