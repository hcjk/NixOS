use crate::{
    BlockDevice, GPT_SIGNATURE, Guid, MBR_SIGNATURE, StorageError, crc32, parse_gpt_header,
    parse_mbr,
};

pub const BIOS_BOOT_TYPE_GUID: Guid = Guid([
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6f, 0x6e, 0x74, 0x4e, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49,
]);
pub const ESP_TYPE_GUID: Guid = Guid([
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
]);
pub const NEXFS_TYPE_GUID: Guid = Guid([
    0x4e, 0x65, 0x78, 0x46, 0x53, 0x00, 0x10, 0x40, 0x80, 0x00, 0x4e, 0x65, 0x78, 0x4f, 0x53, 0x00,
]);

const SECTOR_BYTES: usize = 512;
const SECTOR_BYTES_U32: u32 = 512;
const ALIGNMENT_SECTORS: u64 = 2048;
const GPT_ENTRY_COUNT: u32 = 128;
const GPT_ENTRY_SIZE: u32 = 128;
const GPT_ENTRY_SECTORS: u64 = 32;
const FIRST_USABLE_LBA: u64 = 34;
const BIOS_SECTORS: u64 = 2048;
const ESP_SECTORS: u64 = 64 * 1024 * 1024 / SECTOR_BYTES as u64;
const MIN_DISK_SECTORS: u64 = 128 * 1024 * 1024 / SECTOR_BYTES as u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionSpan {
    pub first_lba: u64,
    pub sector_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallerLayout {
    pub disk_guid: Guid,
    pub bios: PartitionSpan,
    pub esp: PartitionSpan,
    pub root: PartitionSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallerGuids {
    pub disk: Guid,
    pub bios: Guid,
    pub esp: Guid,
    pub root: Guid,
}

pub fn guided_installer_layout(
    total_sectors: u64,
    disk_guid: Guid,
) -> Result<InstallerLayout, StorageError> {
    if total_sectors < MIN_DISK_SECTORS {
        return Err(StorageError::NoSpace);
    }
    let backup_entries_lba = total_sectors
        .checked_sub(1 + GPT_ENTRY_SECTORS)
        .ok_or(StorageError::NoSpace)?;
    let last_usable_lba = backup_entries_lba
        .checked_sub(1)
        .ok_or(StorageError::NoSpace)?;
    let bios = PartitionSpan {
        first_lba: ALIGNMENT_SECTORS,
        sector_count: BIOS_SECTORS,
    };
    let esp = PartitionSpan {
        first_lba: bios.first_lba + bios.sector_count,
        sector_count: ESP_SECTORS,
    };
    let root_first = align_up(
        esp.first_lba
            .checked_add(esp.sector_count)
            .ok_or(StorageError::TooLarge)?,
        ALIGNMENT_SECTORS,
    );
    if root_first >= last_usable_lba {
        return Err(StorageError::NoSpace);
    }
    Ok(InstallerLayout {
        disk_guid,
        bios,
        esp,
        root: PartitionSpan {
            first_lba: root_first,
            sector_count: last_usable_lba - root_first + 1,
        },
    })
}

pub fn write_guided_installer_gpt<D: BlockDevice>(
    device: &mut D,
    guids: InstallerGuids,
) -> Result<InstallerLayout, StorageError> {
    if device.sector_size() != SECTOR_BYTES_U32 {
        return Err(StorageError::UnsupportedSectorSize);
    }
    let total_sectors = device.sector_count();
    let layout = guided_installer_layout(total_sectors, guids.disk)?;
    if guids.disk.is_zero() || guids.bios.is_zero() || guids.esp.is_zero() || guids.root.is_zero() {
        return Err(StorageError::InvalidPartition);
    }
    let backup_header_lba = total_sectors - 1;
    let backup_entries_lba = backup_header_lba - GPT_ENTRY_SECTORS;

    let mut entry_sector = [0_u8; SECTOR_BYTES];
    encode_entry(
        &mut entry_sector[0..128],
        BIOS_BOOT_TYPE_GUID,
        guids.bios,
        layout.bios,
        "NexOS BIOS",
    );
    encode_entry(
        &mut entry_sector[128..256],
        ESP_TYPE_GUID,
        guids.esp,
        layout.esp,
        "NexOS boot",
    );
    encode_entry(
        &mut entry_sector[256..384],
        NEXFS_TYPE_GUID,
        guids.root,
        layout.root,
        "NexOS root",
    );

    let mut entries_crc = Crc32::new();
    for index in 0..GPT_ENTRY_SECTORS {
        let sector = if index == 0 {
            &entry_sector
        } else {
            &[0_u8; SECTOR_BYTES]
        };
        entries_crc.update(sector);
        device.write_sectors(2 + index, sector)?;
        device.write_sectors(backup_entries_lba + index, sector)?;
    }
    let entries_crc = entries_crc.finish();

    let mut protective_mbr = [0_u8; SECTOR_BYTES];
    protective_mbr[450] = 0xee;
    protective_mbr[454..458].copy_from_slice(&1_u32.to_le_bytes());
    let protected_sectors = u32::try_from((total_sectors - 1).min(u64::from(u32::MAX)))
        .map_err(|_| StorageError::TooLarge)?;
    protective_mbr[458..462].copy_from_slice(&protected_sectors.to_le_bytes());
    protective_mbr[510..512].copy_from_slice(&MBR_SIGNATURE);
    device.write_sectors(0, &protective_mbr)?;

    let primary = encode_header(
        1,
        backup_header_lba,
        total_sectors,
        layout.disk_guid,
        2,
        entries_crc,
    );
    device.write_sectors(1, &primary)?;
    let backup = encode_header(
        backup_header_lba,
        1,
        total_sectors,
        layout.disk_guid,
        backup_entries_lba,
        entries_crc,
    );
    device.write_sectors(backup_header_lba, &backup)?;
    device.flush()?;
    verify_guided_installer_gpt(device, &layout)?;
    Ok(layout)
}

pub fn verify_guided_installer_gpt<D: BlockDevice>(
    device: &mut D,
    expected: &InstallerLayout,
) -> Result<(), StorageError> {
    if device.sector_size() != SECTOR_BYTES_U32 {
        return Err(StorageError::UnsupportedSectorSize);
    }
    let mut sector = [0_u8; SECTOR_BYTES];
    device.read_sectors(0, &mut sector)?;
    let mbr = parse_mbr(&sector)?;
    if !mbr
        .iter()
        .flatten()
        .any(|partition| partition.partition_type == 0xee && partition.first_lba == 1)
    {
        return Err(StorageError::InvalidGpt);
    }
    device.read_sectors(1, &mut sector)?;
    let header = parse_gpt_header(&sector)?;
    if header.current_lba != 1
        || header.backup_lba != device.sector_count() - 1
        || header.disk_guid != expected.disk_guid
        || header.entry_count != GPT_ENTRY_COUNT
        || header.entry_size != GPT_ENTRY_SIZE
    {
        return Err(StorageError::InvalidGpt);
    }
    let mut calculated_crc = Crc32::new();
    for index in 0..GPT_ENTRY_SECTORS {
        device.read_sectors(header.entries_lba + index, &mut sector)?;
        calculated_crc.update(&sector);
        if index == 0 {
            verify_entry(&sector[0..128], BIOS_BOOT_TYPE_GUID, expected.bios)?;
            verify_entry(&sector[128..256], ESP_TYPE_GUID, expected.esp)?;
            verify_entry(&sector[256..384], NEXFS_TYPE_GUID, expected.root)?;
        }
    }
    if calculated_crc.finish() != header.entries_crc32 {
        return Err(StorageError::ChecksumMismatch);
    }
    device.read_sectors(header.backup_lba, &mut sector)?;
    let backup = parse_gpt_header(&sector)?;
    if backup.current_lba != header.backup_lba
        || backup.backup_lba != 1
        || backup.disk_guid != header.disk_guid
        || backup.entries_crc32 != header.entries_crc32
    {
        return Err(StorageError::InvalidGpt);
    }
    Ok(())
}

fn encode_entry(
    output: &mut [u8],
    type_guid: Guid,
    unique_guid: Guid,
    span: PartitionSpan,
    name: &str,
) {
    output.fill(0);
    output[0..16].copy_from_slice(&type_guid.0);
    output[16..32].copy_from_slice(&unique_guid.0);
    output[32..40].copy_from_slice(&span.first_lba.to_le_bytes());
    output[40..48].copy_from_slice(&(span.first_lba + span.sector_count - 1).to_le_bytes());
    for (index, character) in name.encode_utf16().take(36).enumerate() {
        output[56 + index * 2..58 + index * 2].copy_from_slice(&character.to_le_bytes());
    }
}

fn verify_entry(
    input: &[u8],
    type_guid: Guid,
    expected: PartitionSpan,
) -> Result<(), StorageError> {
    if input[0..16] != type_guid.0
        || le_u64(input, 32) != expected.first_lba
        || le_u64(input, 40)
            != expected
                .first_lba
                .checked_add(expected.sector_count - 1)
                .ok_or(StorageError::InvalidPartition)?
    {
        return Err(StorageError::InvalidPartition);
    }
    Ok(())
}

fn encode_header(
    current_lba: u64,
    backup_lba: u64,
    total_sectors: u64,
    disk_guid: Guid,
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; SECTOR_BYTES] {
    let mut header = [0_u8; SECTOR_BYTES];
    header[0..8].copy_from_slice(GPT_SIGNATURE);
    header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    header[12..16].copy_from_slice(&92_u32.to_le_bytes());
    header[24..32].copy_from_slice(&current_lba.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[40..48].copy_from_slice(&FIRST_USABLE_LBA.to_le_bytes());
    header[48..56].copy_from_slice(&(total_sectors - GPT_ENTRY_SECTORS - 2).to_le_bytes());
    header[56..72].copy_from_slice(&disk_guid.0);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&GPT_ENTRY_COUNT.to_le_bytes());
    header[84..88].copy_from_slice(&GPT_ENTRY_SIZE.to_le_bytes());
    header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let checksum = crc32(&header[..92]);
    header[16..20].copy_from_slice(&checksum.to_le_bytes());
    header
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.saturating_add(alignment - 1) / alignment * alignment
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

struct Crc32(u32);

impl Crc32 {
    const fn new() -> Self {
        Self(!0)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(self.0 & 1)));
            }
        }
    }

    const fn finish(self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    const GUIDS: InstallerGuids = InstallerGuids {
        disk: Guid([1; 16]),
        bios: Guid([2; 16]),
        esp: Guid([3; 16]),
        root: Guid([4; 16]),
    };

    #[test]
    fn writes_and_verifies_guided_installer_gpt() {
        let mut disk = MemoryBlockDevice::new(MIN_DISK_SECTORS, 512).unwrap();
        let layout = write_guided_installer_gpt(&mut disk, GUIDS).unwrap();
        assert_eq!(layout.bios.first_lba, 2048);
        assert_eq!(layout.esp.sector_count, 131_072);
        assert!(layout.root.sector_count > 100_000);
        verify_guided_installer_gpt(&mut disk, &layout).unwrap();
    }

    #[test]
    fn refuses_small_disk_and_zero_identifiers() {
        assert_eq!(
            guided_installer_layout(1000, GUIDS.disk),
            Err(StorageError::NoSpace)
        );
        let mut disk = MemoryBlockDevice::new(MIN_DISK_SECTORS, 512).unwrap();
        let mut invalid = GUIDS;
        invalid.root = Guid::default();
        assert_eq!(
            write_guided_installer_gpt(&mut disk, invalid),
            Err(StorageError::InvalidPartition)
        );
    }
}
