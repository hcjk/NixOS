use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nexos_storage::{
    BlockDevice, Guid, MemoryBlockDevice, PartitionTableKind, crc32, read_partition_table,
};

pub const SECTOR_SIZE: usize = 512;
pub const SECTOR_SIZE_U32: u32 = 512;
pub const SECTOR_SIZE_U64: u64 = 512;
pub const MIB: usize = 1024 * 1024;
pub const ALIGNMENT_SECTORS: u64 = 2048;
pub const GPT_ENTRY_COUNT: usize = 128;
pub const GPT_ENTRY_COUNT_U32: u32 = 128;
pub const GPT_ENTRY_BYTES: usize = 128;
pub const GPT_ENTRY_BYTES_U32: u32 = 128;
pub const GPT_ENTRY_SECTORS: u64 = 32;
pub const GPT_FIRST_USABLE_LBA: u64 = 34;
pub const MIN_DISK_MIB: usize = 64;

pub const ESP_TYPE_GUID: Guid = Guid([
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
]);
pub const BIOS_BOOT_TYPE_GUID: Guid = Guid([
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6f, 0x6e, 0x74, 0x4e, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49,
]);
pub const NEXFS_TYPE_GUID: Guid = Guid([
    0x4e, 0x65, 0x78, 0x46, 0x53, 0x00, 0x10, 0x40, 0x80, 0x00, 0x4e, 0x65, 0x78, 0x4f, 0x53, 0x00,
]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableKind {
    Gpt,
    Mbr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuidedMode {
    Combined,
    Uefi,
    Bios,
}

impl GuidedMode {
    #[must_use]
    pub const fn has_uefi(self) -> bool {
        matches!(self, Self::Combined | Self::Uefi)
    }

    #[must_use]
    pub const fn has_bios(self) -> bool {
        matches!(self, Self::Combined | Self::Bios)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionRole {
    BiosBoot,
    EfiSystem,
    NexFs,
    Data,
}

impl PartitionRole {
    #[must_use]
    pub const fn type_guid(self) -> Guid {
        match self {
            Self::BiosBoot => BIOS_BOOT_TYPE_GUID,
            Self::EfiSystem => ESP_TYPE_GUID,
            Self::NexFs => NEXFS_TYPE_GUID,
            Self::Data => Guid([
                0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47,
                0x7d, 0xe4,
            ]),
        }
    }

    #[must_use]
    pub const fn mbr_type(self) -> u8 {
        match self {
            Self::BiosBoot => 0xda,
            Self::EfiSystem => 0x0c,
            Self::NexFs => 0xa9,
            Self::Data => 0x83,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionPlan {
    pub role: PartitionRole,
    pub name: String,
    pub first_lba: u64,
    pub sector_count: u64,
    pub unique_guid: Guid,
    pub bootable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutPlan {
    pub kind: TableKind,
    pub total_sectors: u64,
    pub disk_guid: Guid,
    pub partitions: Vec<PartitionPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemKind {
    Unknown,
    Fat32,
    NexFs,
}

impl FilesystemKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Fat32 => "FAT32",
            Self::NexFs => "NexFS",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionReport {
    pub index: u32,
    pub first_lba: u64,
    pub sector_count: u64,
    pub name: String,
    pub role: PartitionRole,
    pub filesystem: FilesystemKind,
    pub bootable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskReport {
    pub bytes: u64,
    pub sectors: u64,
    pub sector_size: u32,
    pub table: TableKind,
    pub partitions: Vec<PartitionReport>,
}

pub fn validate_image_path(path: &Path) -> Result<(), String> {
    let text = path.as_os_str().to_string_lossy();
    let lower = text.to_ascii_lowercase();
    if lower.starts_with(r"\\.\")
        || lower.starts_with(r"\\?\")
        || lower == "/dev"
        || lower.starts_with("/dev/")
    {
        return Err("raw physical-disk paths are disabled; use an ordinary image file".into());
    }
    if text.trim().is_empty() {
        return Err("target image path cannot be empty".into());
    }
    Ok(())
}

pub fn require_confirmation(target: &Path, yes: bool, confirmation: &str) -> Result<(), String> {
    if !yes {
        return Err("destructive operation requires --yes".into());
    }
    let expected = target.as_os_str().to_string_lossy();
    if confirmation != expected {
        return Err(format!(
            "confirmation mismatch; type the complete target exactly: {expected}"
        ));
    }
    Ok(())
}

pub fn create_blank_bytes(size_mib: usize) -> Result<Vec<u8>, String> {
    if size_mib < MIN_DISK_MIB {
        return Err(format!("disk image must be at least {MIN_DISK_MIB} MiB"));
    }
    let byte_count = size_mib
        .checked_mul(MIB)
        .ok_or("requested disk image is too large")?;
    Ok(vec![0; byte_count])
}

pub fn guided_layout(size_mib: usize, mode: GuidedMode) -> Result<LayoutPlan, String> {
    let byte_count = size_mib
        .checked_mul(MIB)
        .ok_or("requested disk image is too large")?;
    if size_mib < 96 {
        return Err("guided installation needs at least 96 MiB".into());
    }
    let total_sectors =
        u64::try_from(byte_count / SECTOR_SIZE).map_err(|_| "requested disk image is too large")?;
    let mut partitions = Vec::new();
    let mut next_lba = ALIGNMENT_SECTORS;
    if mode.has_bios() && mode != GuidedMode::Bios {
        partitions.push(PartitionPlan {
            role: PartitionRole::BiosBoot,
            name: "NexOS BIOS".into(),
            first_lba: next_lba,
            sector_count: ALIGNMENT_SECTORS,
            unique_guid: generate_guid(),
            bootable: false,
        });
        next_lba += ALIGNMENT_SECTORS;
    }
    if mode.has_uefi() || mode == GuidedMode::Bios {
        let esp_sectors = 32_u64 * 1024 * 1024 / SECTOR_SIZE_U64;
        partitions.push(PartitionPlan {
            role: PartitionRole::EfiSystem,
            name: "NexOS boot".into(),
            first_lba: next_lba,
            sector_count: esp_sectors,
            unique_guid: generate_guid(),
            bootable: mode == GuidedMode::Bios,
        });
        next_lba = align_up(next_lba + esp_sectors, ALIGNMENT_SECTORS);
    }
    let last_usable = if mode == GuidedMode::Bios {
        total_sectors
            .checked_sub(1)
            .ok_or("disk image is too small")?
    } else {
        total_sectors
            .checked_sub(GPT_FIRST_USABLE_LBA)
            .ok_or("disk image is too small")?
    };
    if next_lba >= last_usable {
        return Err("disk image has no space for a NexFS root partition".into());
    }
    partitions.push(PartitionPlan {
        role: PartitionRole::NexFs,
        name: "NexOS root".into(),
        first_lba: next_lba,
        sector_count: last_usable - next_lba + 1,
        unique_guid: generate_guid(),
        bootable: false,
    });
    let kind = if mode == GuidedMode::Bios {
        TableKind::Mbr
    } else {
        TableKind::Gpt
    };
    let plan = LayoutPlan {
        kind,
        total_sectors,
        disk_guid: generate_guid(),
        partitions,
    };
    validate_layout(&plan)?;
    Ok(plan)
}

pub fn empty_layout(bytes: usize, kind: TableKind) -> Result<LayoutPlan, String> {
    if bytes < MIN_DISK_MIB * MIB || !bytes.is_multiple_of(SECTOR_SIZE) {
        return Err("disk image must be sector-aligned and at least 64 MiB".into());
    }
    Ok(LayoutPlan {
        kind,
        total_sectors: u64::try_from(bytes / SECTOR_SIZE).map_err(|_| "disk image is too large")?,
        disk_guid: generate_guid(),
        partitions: Vec::new(),
    })
}

pub fn write_layout(image: &mut [u8], plan: &LayoutPlan) -> Result<(), String> {
    if image.len() / SECTOR_SIZE
        != usize::try_from(plan.total_sectors).map_err(|_| "disk is too large")?
    {
        return Err("layout size does not match disk image".into());
    }
    validate_layout(plan)?;
    image[..SECTOR_SIZE].fill(0);
    match plan.kind {
        TableKind::Gpt => write_gpt(image, plan),
        TableKind::Mbr => write_mbr(image, plan),
    }
}

pub fn inspect_bytes(bytes: &[u8]) -> Result<DiskReport, String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(SECTOR_SIZE) {
        return Err("disk image is not 512-byte sector aligned".into());
    }
    let mut disk = MemoryBlockDevice::from_bytes(bytes.to_vec(), SECTOR_SIZE_U32)
        .map_err(|error| format!("invalid disk image: {error:?}"))?;
    let table = read_partition_table(&mut disk)
        .map_err(|error| format!("cannot read partition table: {error:?}"))?;
    let kind = match table.kind {
        PartitionTableKind::Gpt => TableKind::Gpt,
        PartitionTableKind::Mbr => TableKind::Mbr,
    };
    let mut partitions = Vec::new();
    for partition in table.partitions {
        let role = partition
            .type_guid
            .map(role_from_guid)
            .or_else(|| partition.mbr_type.map(role_from_mbr))
            .unwrap_or(PartitionRole::Data);
        let filesystem = detect_filesystem(bytes, partition.first_lba, partition.sector_count);
        partitions.push(PartitionReport {
            index: partition.index,
            first_lba: partition.first_lba,
            sector_count: partition.sector_count,
            name: partition.name,
            role,
            filesystem,
            bootable: partition.bootable,
        });
    }
    Ok(DiskReport {
        bytes: u64::try_from(bytes.len()).map_err(|_| "disk image is too large")?,
        sectors: disk.sector_count(),
        sector_size: SECTOR_SIZE_U32,
        table: kind,
        partitions,
    })
}

pub fn inspect_image(path: &Path) -> Result<DiskReport, String> {
    validate_existing_image(path)?;
    let bytes = fs::read(path).map_err(io_error)?;
    inspect_bytes(&bytes)
}

#[must_use]
pub fn layout_from_report(report: &DiskReport) -> LayoutPlan {
    LayoutPlan {
        kind: report.table,
        total_sectors: report.sectors,
        disk_guid: generate_guid(),
        partitions: report
            .partitions
            .iter()
            .map(|partition| PartitionPlan {
                role: partition.role,
                name: partition.name.clone(),
                first_lba: partition.first_lba,
                sector_count: partition.sector_count,
                unique_guid: generate_guid(),
                bootable: partition.bootable,
            })
            .collect(),
    }
}

pub fn format_partition(
    image: &mut [u8],
    partition: &PartitionReport,
    filesystem: FilesystemKind,
    uuid: [u8; 16],
) -> Result<(), String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let partition_bytes = &mut image[range];
    match filesystem {
        FilesystemKind::Fat32 => {
            if partition_bytes.len() < 32 * MIB {
                return Err("FAT32 partition must be at least 32 MiB".into());
            }
            let mut cursor = Cursor::new(partition_bytes);
            fatfs::format_volume(
                &mut cursor,
                fatfs::FormatVolumeOptions::new()
                    .fat_type(fatfs::FatType::Fat32)
                    .volume_label(*b"NEXOS BOOT "),
            )
            .map_err(|error| format!("FAT32 format failed: {error}"))?;
        }
        FilesystemKind::NexFs => {
            let mut disk = MemoryBlockDevice::from_bytes(partition_bytes.to_vec(), SECTOR_SIZE_U32)
                .map_err(|error| format!("invalid partition: {error:?}"))?;
            nexfs::format(&mut disk, uuid)
                .map_err(|error| format!("NexFS format failed: {error:?}"))?;
            partition_bytes.copy_from_slice(disk.bytes());
        }
        FilesystemKind::Unknown => return Err("unsupported filesystem".into()),
    }
    Ok(())
}

pub fn check_partition(image: &[u8], partition: &PartitionReport) -> Result<String, String> {
    let range = partition_range(image.len(), partition.first_lba, partition.sector_count)?;
    let partition_bytes = &image[range];
    match detect_filesystem(image, partition.first_lba, partition.sector_count) {
        FilesystemKind::NexFs => {
            let mut disk = MemoryBlockDevice::from_bytes(partition_bytes.to_vec(), SECTOR_SIZE_U32)
                .map_err(|error| format!("invalid partition: {error:?}"))?;
            let report = nexfs::check_detailed(&mut disk)
                .map_err(|error| format!("NexFS check failed: {error:?}"))?;
            Ok(format!(
                "clean NexFS v{}, {} blocks, {} inodes",
                report.superblock.version, report.superblock.total_blocks, report.allocated_inodes
            ))
        }
        FilesystemKind::Fat32 => {
            let cursor = Cursor::new(partition_bytes.to_vec());
            let filesystem = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
                .map_err(|error| format!("FAT32 check failed: {error}"))?;
            let stats = filesystem
                .stats()
                .map_err(|error| format!("FAT32 stats failed: {error}"))?;
            Ok(format!(
                "readable FAT32, {} free clusters",
                stats.free_clusters()
            ))
        }
        FilesystemKind::Unknown => Err("partition has no supported filesystem".into()),
    }
}

pub fn write_transactional(path: &Path, bytes: &[u8]) -> Result<(), String> {
    validate_image_path(path)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = prepared_path(path)?;
    write_prepared(&temporary, bytes)?;
    if let Err(error) = inspect_image(&temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("prepared image verification failed: {error}"));
    }
    commit_prepared(path, &temporary)
}

pub fn prepared_path(path: &Path) -> Result<PathBuf, String> {
    let temporary = sibling_path(path, "nexos-new")?;
    let backup = sibling_path(path, "nexos-backup")?;
    if temporary.exists() || backup.exists() {
        return Err("stale installer temporary or backup file exists".into());
    }
    Ok(temporary)
}

pub fn write_prepared(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    write_new(path, bytes)
}

pub fn commit_prepared(path: &Path, temporary: &Path) -> Result<(), String> {
    let backup = sibling_path(path, "nexos-backup")?;
    if !path.exists() {
        fs::rename(temporary, path).map_err(io_error)?;
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        let _ = fs::remove_file(temporary);
        return Err("existing target must be a regular non-symlink image file".into());
    }
    fs::rename(path, &backup).map_err(io_error)?;
    if let Err(error) = fs::rename(temporary, path) {
        let _ = fs::rename(&backup, path);
        return Err(format!("cannot commit prepared image: {error}"));
    }
    if let Err(error) = inspect_image(path) {
        let failed = sibling_path(path, "nexos-failed")?;
        let _ = fs::rename(path, &failed);
        let _ = fs::rename(&backup, path);
        return Err(format!(
            "installed image verification failed; original restored: {error}"
        ));
    }
    fs::remove_file(&backup).map_err(io_error)
}

pub fn validate_existing_image(path: &Path) -> Result<(), String> {
    validate_image_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("target must be a regular non-symlink image file".into());
    }
    Ok(())
}

pub fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    validate_existing_image(path)?;
    fs::read(path).map_err(io_error)
}

pub fn partition_range(
    image_bytes: usize,
    first_lba: u64,
    sector_count: u64,
) -> Result<std::ops::Range<usize>, String> {
    let start = first_lba
        .checked_mul(SECTOR_SIZE_U64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("partition offset is too large")?;
    let length = sector_count
        .checked_mul(SECTOR_SIZE_U64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("partition is too large")?;
    let end = start.checked_add(length).ok_or("partition is too large")?;
    if start >= end || end > image_bytes {
        return Err("partition is outside the disk image".into());
    }
    Ok(start..end)
}

#[must_use]
pub fn generate_guid() -> Guid {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos_bytes = nanos.to_le_bytes();
    let nanos_low = u64::from_le_bytes(std::array::from_fn(|index| nanos_bytes[index]));
    let nanos_high = u64::from_le_bytes(std::array::from_fn(|index| nanos_bytes[index + 8]));
    let seed = nanos_low
        ^ nanos_high
        ^ counter.rotate_left(19)
        ^ u64::from(std::process::id()).rotate_left(41);
    let first = mix_guid_word(seed);
    let second = mix_guid_word(seed ^ counter ^ 0xd6e8_feb8_6659_fd93);
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&first.to_le_bytes());
    bytes[8..].copy_from_slice(&second.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Guid(bytes)
}

const fn mix_guid_word(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[must_use]
pub fn hex_guid(guid: Guid) -> String {
    let bytes = guid.0;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn validate_layout(plan: &LayoutPlan) -> Result<(), String> {
    if plan.total_sectors < u64::try_from(MIN_DISK_MIB * MIB / SECTOR_SIZE).unwrap_or(u64::MAX) {
        return Err("disk layout is too small".into());
    }
    if plan.kind == TableKind::Mbr && plan.partitions.len() > 4 {
        return Err("MBR supports at most four primary partitions".into());
    }
    if plan.kind == TableKind::Gpt && plan.partitions.len() > GPT_ENTRY_COUNT {
        return Err("GPT partition entry array is full".into());
    }
    let first_usable = if plan.kind == TableKind::Gpt {
        GPT_FIRST_USABLE_LBA
    } else {
        1
    };
    let last_usable = if plan.kind == TableKind::Gpt {
        plan.total_sectors
            .checked_sub(GPT_FIRST_USABLE_LBA)
            .ok_or("disk layout is too small")?
    } else {
        plan.total_sectors - 1
    };
    for (index, partition) in plan.partitions.iter().enumerate() {
        let end = partition
            .first_lba
            .checked_add(partition.sector_count)
            .and_then(|value| value.checked_sub(1))
            .ok_or("invalid empty or overflowing partition")?;
        if partition.sector_count == 0 || partition.first_lba < first_usable || end > last_usable {
            return Err(format!(
                "partition {} is outside usable disk space",
                index + 1
            ));
        }
        for other in &plan.partitions[index + 1..] {
            let other_end = other
                .first_lba
                .checked_add(other.sector_count)
                .ok_or("partition end overflow")?;
            if partition.first_lba < other_end
                && other.first_lba < partition.first_lba + partition.sector_count
            {
                return Err("partitions overlap".into());
            }
        }
    }
    Ok(())
}

fn write_mbr(image: &mut [u8], plan: &LayoutPlan) -> Result<(), String> {
    let sector = &mut image[..SECTOR_SIZE];
    sector.fill(0);
    for (index, partition) in plan.partitions.iter().enumerate() {
        let first_lba =
            u32::try_from(partition.first_lba).map_err(|_| "MBR partition starts beyond 2 TiB")?;
        let sector_count = u32::try_from(partition.sector_count)
            .map_err(|_| "MBR partition is larger than 2 TiB")?;
        let offset = 446 + index * 16;
        let entry = &mut sector[offset..offset + 16];
        entry[0] = if partition.bootable { 0x80 } else { 0 };
        entry[1..4].fill(0xff);
        entry[4] = partition.role.mbr_type();
        entry[5..8].fill(0xff);
        entry[8..12].copy_from_slice(&first_lba.to_le_bytes());
        entry[12..16].copy_from_slice(&sector_count.to_le_bytes());
    }
    sector[510..512].copy_from_slice(&nexos_storage::MBR_SIGNATURE);
    Ok(())
}

fn write_gpt(image: &mut [u8], plan: &LayoutPlan) -> Result<(), String> {
    let total_sectors = plan.total_sectors;
    let backup_header_lba = total_sectors - 1;
    let backup_entries_lba = backup_header_lba - GPT_ENTRY_SECTORS;
    let last_usable_lba = backup_entries_lba - 1;
    let mut entries = vec![0_u8; GPT_ENTRY_COUNT * GPT_ENTRY_BYTES];
    for (index, partition) in plan.partitions.iter().enumerate() {
        let entry = &mut entries[index * GPT_ENTRY_BYTES..(index + 1) * GPT_ENTRY_BYTES];
        entry[0..16].copy_from_slice(&partition.role.type_guid().0);
        entry[16..32].copy_from_slice(&partition.unique_guid.0);
        entry[32..40].copy_from_slice(&partition.first_lba.to_le_bytes());
        let last_lba = partition.first_lba + partition.sector_count - 1;
        entry[40..48].copy_from_slice(&last_lba.to_le_bytes());
        encode_gpt_name(&partition.name, &mut entry[56..128]);
    }
    let entries_crc = crc32(&entries);
    let primary_entries = partition_range(image.len(), 2, GPT_ENTRY_SECTORS)?;
    image[primary_entries].copy_from_slice(&entries);
    let backup_entries = partition_range(image.len(), backup_entries_lba, GPT_ENTRY_SECTORS)?;
    image[backup_entries].copy_from_slice(&entries);

    let mut protective = [0_u8; SECTOR_SIZE];
    let sectors = u32::try_from((total_sectors - 1).min(u64::from(u32::MAX)))
        .map_err(|_| "protective MBR sector count is too large")?;
    protective[446 + 4] = 0xee;
    protective[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
    protective[446 + 12..446 + 16].copy_from_slice(&sectors.to_le_bytes());
    protective[510..512].copy_from_slice(&nexos_storage::MBR_SIGNATURE);
    image[..SECTOR_SIZE].copy_from_slice(&protective);

    let primary = gpt_header(
        1,
        backup_header_lba,
        GPT_FIRST_USABLE_LBA,
        last_usable_lba,
        plan.disk_guid,
        2,
        entries_crc,
    );
    let primary_range = partition_range(image.len(), 1, 1)?;
    image[primary_range].copy_from_slice(&primary);
    let backup = gpt_header(
        backup_header_lba,
        1,
        GPT_FIRST_USABLE_LBA,
        last_usable_lba,
        plan.disk_guid,
        backup_entries_lba,
        entries_crc,
    );
    let backup_range = partition_range(image.len(), backup_header_lba, 1)?;
    image[backup_range].copy_from_slice(&backup);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn gpt_header(
    current_lba: u64,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: Guid,
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; SECTOR_SIZE] {
    let mut header = [0_u8; SECTOR_SIZE];
    header[0..8].copy_from_slice(nexos_storage::GPT_SIGNATURE);
    header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    header[12..16].copy_from_slice(&92_u32.to_le_bytes());
    header[24..32].copy_from_slice(&current_lba.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[40..48].copy_from_slice(&first_usable_lba.to_le_bytes());
    header[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    header[56..72].copy_from_slice(&disk_guid.0);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&GPT_ENTRY_COUNT_U32.to_le_bytes());
    header[84..88].copy_from_slice(&GPT_ENTRY_BYTES_U32.to_le_bytes());
    header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = crc32(&header[..92]);
    header[16..20].copy_from_slice(&header_crc.to_le_bytes());
    header
}

fn encode_gpt_name(name: &str, output: &mut [u8]) {
    output.fill(0);
    for (index, value) in name.encode_utf16().take(output.len() / 2).enumerate() {
        output[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
}

fn detect_filesystem(image: &[u8], first_lba: u64, sectors: u64) -> FilesystemKind {
    let Ok(range) = partition_range(image.len(), first_lba, sectors) else {
        return FilesystemKind::Unknown;
    };
    let bytes = &image[range];
    if bytes.len() >= nexfs::BLOCK_SIZE + nexfs::MAGIC.len()
        && bytes[nexfs::BLOCK_SIZE..nexfs::BLOCK_SIZE + nexfs::MAGIC.len()] == nexfs::MAGIC
    {
        return FilesystemKind::NexFs;
    }
    if bytes.len() >= SECTOR_SIZE
        && bytes[510..512] == nexos_storage::MBR_SIGNATURE
        && (bytes.get(82..87) == Some(b"FAT32") || bytes.get(54..59) == Some(b"FAT16"))
    {
        return FilesystemKind::Fat32;
    }
    FilesystemKind::Unknown
}

fn role_from_guid(guid: Guid) -> PartitionRole {
    if guid.0 == ESP_TYPE_GUID.0 {
        PartitionRole::EfiSystem
    } else if guid.0 == BIOS_BOOT_TYPE_GUID.0 {
        PartitionRole::BiosBoot
    } else if guid.0 == NEXFS_TYPE_GUID.0 {
        PartitionRole::NexFs
    } else {
        PartitionRole::Data
    }
}

const fn role_from_mbr(value: u8) -> PartitionRole {
    match value {
        0x0b | 0x0c | 0xef => PartitionRole::EfiSystem,
        0xa9 => PartitionRole::NexFs,
        0xda => PartitionRole::BiosBoot,
        _ => PartitionRole::Data,
    }
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.saturating_add(alignment - 1) / alignment * alignment
}

fn sibling_path(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or("target image needs a file name")?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{file_name}.{suffix}")))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_raw_devices_and_confirmation_typos() {
        assert!(validate_image_path(Path::new(r"\\.\PhysicalDrive0")).is_err());
        assert!(validate_image_path(Path::new(r"\\?\Volume{example}")).is_err());
        assert!(validate_image_path(Path::new("/dev")).is_err());
        assert!(validate_image_path(Path::new("/dev/nvme0n1")).is_err());
        let target = Path::new("disk.img");
        assert!(require_confirmation(target, true, "disk.img").is_ok());
        assert!(require_confirmation(target, true, "other.img").is_err());
        assert!(require_confirmation(target, false, "disk.img").is_err());
    }

    #[test]
    fn guided_combined_gpt_round_trips() {
        let mut image = create_blank_bytes(128).unwrap();
        let layout = guided_layout(128, GuidedMode::Combined).unwrap();
        write_layout(&mut image, &layout).unwrap();
        let report = inspect_bytes(&image).unwrap();
        assert_eq!(report.table, TableKind::Gpt);
        assert_eq!(report.partitions.len(), 3);
        assert_eq!(report.partitions[0].role, PartitionRole::BiosBoot);
        assert_eq!(report.partitions[1].role, PartitionRole::EfiSystem);
        assert_eq!(report.partitions[2].role, PartitionRole::NexFs);
    }

    #[test]
    fn guided_bios_mbr_round_trips() {
        let mut image = create_blank_bytes(128).unwrap();
        let layout = guided_layout(128, GuidedMode::Bios).unwrap();
        write_layout(&mut image, &layout).unwrap();
        let report = inspect_bytes(&image).unwrap();
        assert_eq!(report.table, TableKind::Mbr);
        assert_eq!(report.partitions.len(), 2);
        assert!(report.partitions[0].bootable);
    }

    #[test]
    fn formats_and_checks_nexfs_partition() {
        let mut image = create_blank_bytes(128).unwrap();
        let layout = guided_layout(128, GuidedMode::Combined).unwrap();
        write_layout(&mut image, &layout).unwrap();
        let report = inspect_bytes(&image).unwrap();
        let root = report
            .partitions
            .iter()
            .find(|partition| partition.role == PartitionRole::NexFs)
            .unwrap();
        format_partition(&mut image, root, FilesystemKind::NexFs, generate_guid().0).unwrap();
        let updated = inspect_bytes(&image).unwrap();
        let root = updated
            .partitions
            .iter()
            .find(|partition| partition.role == PartitionRole::NexFs)
            .unwrap();
        assert_eq!(root.filesystem, FilesystemKind::NexFs);
        assert!(
            check_partition(&image, root)
                .unwrap()
                .contains("clean NexFS")
        );
    }
}
