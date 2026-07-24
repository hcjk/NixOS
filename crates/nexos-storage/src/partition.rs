#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use crate::crc32;
use crate::{BlockDevice, GPT_SIGNATURE, MBR_SIGNATURE, StorageError};

const MAX_GPT_ENTRIES: u32 = 4096;
const MAX_GPT_ENTRY_SIZE: u32 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MbrPartition {
    pub bootable: bool,
    pub partition_type: u8,
    pub first_lba: u32,
    pub sector_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GptHeader {
    pub revision: u32,
    pub header_size: u32,
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub disk_guid: Guid,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc32: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 16]
    }
}

#[cfg(feature = "alloc")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionTableKind {
    Mbr,
    Gpt,
}

#[cfg(feature = "alloc")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Partition {
    pub index: u32,
    pub first_lba: u64,
    pub sector_count: u64,
    pub bootable: bool,
    pub mbr_type: Option<u8>,
    pub type_guid: Option<Guid>,
    pub unique_guid: Option<Guid>,
    pub attributes: u64,
    pub name: String,
}

#[cfg(feature = "alloc")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionTable {
    pub kind: PartitionTableKind,
    pub disk_guid: Option<Guid>,
    pub partitions: Vec<Partition>,
}

pub struct PartitionDevice<'a, D> {
    inner: &'a mut D,
    first_lba: u64,
    sector_count: u64,
}

impl<'a, D: BlockDevice> PartitionDevice<'a, D> {
    pub fn new(inner: &'a mut D, first_lba: u64, sector_count: u64) -> Result<Self, StorageError> {
        if sector_count == 0
            || first_lba
                .checked_add(sector_count)
                .is_none_or(|end| end > inner.sector_count())
        {
            return Err(StorageError::InvalidPartition);
        }
        Ok(Self {
            inner,
            first_lba,
            sector_count,
        })
    }

    #[must_use]
    pub const fn first_lba(&self) -> u64 {
        self.first_lba
    }

    fn translated_lba(&self, lba: u64, byte_length: usize) -> Result<u64, StorageError> {
        let sector_size =
            usize::try_from(self.inner.sector_size()).map_err(|_| StorageError::TooLarge)?;
        if byte_length == 0 || !byte_length.is_multiple_of(sector_size) {
            return Err(StorageError::InvalidBuffer);
        }
        let count = u64::try_from(byte_length / sector_size).map_err(|_| StorageError::TooLarge)?;
        if lba
            .checked_add(count)
            .is_none_or(|end| end > self.sector_count)
        {
            return Err(StorageError::OutOfBounds);
        }
        self.first_lba
            .checked_add(lba)
            .ok_or(StorageError::OutOfBounds)
    }
}

impl<D: BlockDevice> BlockDevice for PartitionDevice<'_, D> {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let translated = self.translated_lba(lba, output.len())?;
        self.inner.read_sectors(translated, output)
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        let translated = self.translated_lba(lba, input.len())?;
        self.inner.write_sectors(translated, input)
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        self.inner.flush()
    }
}

pub fn parse_mbr(sector: &[u8]) -> Result<[Option<MbrPartition>; 4], StorageError> {
    if sector.len() < 512 || sector[510..512] != MBR_SIGNATURE {
        return Err(StorageError::InvalidMbr);
    }
    let mut result = [None; 4];
    for (index, slot) in result.iter_mut().enumerate() {
        let offset = 446 + index * 16;
        let entry = &sector[offset..offset + 16];
        if !matches!(entry[0], 0 | 0x80) {
            return Err(StorageError::InvalidMbr);
        }
        let partition_type = entry[4];
        let first_lba = le_u32(entry, 8);
        let sector_count = le_u32(entry, 12);
        if partition_type == 0 && sector_count == 0 {
            continue;
        }
        if partition_type == 0 || sector_count == 0 {
            return Err(StorageError::InvalidMbr);
        }
        *slot = Some(MbrPartition {
            bootable: entry[0] == 0x80,
            partition_type,
            first_lba,
            sector_count,
        });
    }
    Ok(result)
}

pub fn parse_gpt_header(sector: &[u8]) -> Result<GptHeader, StorageError> {
    if sector.len() < 92 || &sector[0..8] != GPT_SIGNATURE {
        return Err(StorageError::InvalidGpt);
    }
    let header_size = le_u32(sector, 12);
    let header_length = usize::try_from(header_size).map_err(|_| StorageError::InvalidGpt)?;
    if !(92..=sector.len()).contains(&header_length) {
        return Err(StorageError::InvalidGpt);
    }
    let expected_crc = le_u32(sector, 16);
    if gpt_header_crc32(&sector[..header_length]) != expected_crc {
        return Err(StorageError::ChecksumMismatch);
    }
    let parsed = GptHeader {
        revision: le_u32(sector, 8),
        header_size,
        current_lba: le_u64(sector, 24),
        backup_lba: le_u64(sector, 32),
        first_usable_lba: le_u64(sector, 40),
        last_usable_lba: le_u64(sector, 48),
        disk_guid: Guid(
            sector[56..72]
                .try_into()
                .map_err(|_| StorageError::InvalidGpt)?,
        ),
        entries_lba: le_u64(sector, 72),
        entry_count: le_u32(sector, 80),
        entry_size: le_u32(sector, 84),
        entries_crc32: le_u32(sector, 88),
    };
    if parsed.revision != 0x0001_0000
        || parsed.current_lba == parsed.backup_lba
        || parsed.first_usable_lba > parsed.last_usable_lba
        || parsed.entry_count == 0
        || parsed.entry_count > MAX_GPT_ENTRIES
        || !(128..=MAX_GPT_ENTRY_SIZE).contains(&parsed.entry_size)
        || !parsed.entry_size.is_multiple_of(8)
    {
        return Err(StorageError::InvalidGpt);
    }
    Ok(parsed)
}

fn gpt_header_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        crc ^= u32::from(if (16..20).contains(&index) { 0 } else { byte });
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(feature = "alloc")]
pub fn read_partition_table<D: BlockDevice>(
    device: &mut D,
) -> Result<PartitionTable, StorageError> {
    let sector_size = usize::try_from(device.sector_size()).map_err(|_| StorageError::TooLarge)?;
    if sector_size < 512 {
        return Err(StorageError::UnsupportedSectorSize);
    }
    let mut sector = vec![0; sector_size];
    device.read_sectors(0, &mut sector)?;
    let mbr = parse_mbr(&sector)?;
    if mbr
        .iter()
        .flatten()
        .any(|partition| partition.partition_type == 0xee)
    {
        return read_gpt(device, sector_size);
    }
    read_mbr_table(device.sector_count(), mbr)
}

#[cfg(feature = "alloc")]
fn read_mbr_table(
    disk_sectors: u64,
    entries: [Option<MbrPartition>; 4],
) -> Result<PartitionTable, StorageError> {
    let mut partitions = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let Some(entry) = entry else {
            continue;
        };
        if entry.partition_type == 0xee {
            return Err(StorageError::InvalidGpt);
        }
        let first_lba = u64::from(entry.first_lba);
        let sector_count = u64::from(entry.sector_count);
        validate_partition_bounds(first_lba, sector_count, disk_sectors)?;
        partitions.push(Partition {
            index: u32::try_from(index + 1).map_err(|_| StorageError::TooLarge)?,
            first_lba,
            sector_count,
            bootable: entry.bootable,
            mbr_type: Some(entry.partition_type),
            type_guid: None,
            unique_guid: None,
            attributes: 0,
            name: String::new(),
        });
    }
    validate_no_overlap(&partitions)?;
    Ok(PartitionTable {
        kind: PartitionTableKind::Mbr,
        disk_guid: None,
        partitions,
    })
}

#[cfg(feature = "alloc")]
fn read_gpt<D: BlockDevice>(
    device: &mut D,
    sector_size: usize,
) -> Result<PartitionTable, StorageError> {
    let mut sector = vec![0; sector_size];
    device.read_sectors(1, &mut sector)?;
    let header = parse_gpt_header(&sector)?;
    let disk_sectors = device.sector_count();
    if header.current_lba != 1
        || header.backup_lba >= disk_sectors
        || header.last_usable_lba >= disk_sectors
        || header.entries_lba < 2
    {
        return Err(StorageError::InvalidGpt);
    }
    let entries_bytes = u64::from(header.entry_count)
        .checked_mul(u64::from(header.entry_size))
        .ok_or(StorageError::TooLarge)?;
    let entry_sectors = entries_bytes
        .checked_add(u64::try_from(sector_size - 1).map_err(|_| StorageError::TooLarge)?)
        .ok_or(StorageError::TooLarge)?
        / u64::try_from(sector_size).map_err(|_| StorageError::TooLarge)?;
    if header
        .entries_lba
        .checked_add(entry_sectors)
        .is_none_or(|end| end > disk_sectors || end > header.first_usable_lba)
    {
        return Err(StorageError::InvalidGpt);
    }
    let transfer_bytes = entry_sectors
        .checked_mul(u64::try_from(sector_size).map_err(|_| StorageError::TooLarge)?)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(StorageError::TooLarge)?;
    let mut bytes = vec![0; transfer_bytes];
    device.read_sectors(header.entries_lba, &mut bytes)?;
    let entries_length = usize::try_from(entries_bytes).map_err(|_| StorageError::TooLarge)?;
    if crc32(&bytes[..entries_length]) != header.entries_crc32 {
        return Err(StorageError::ChecksumMismatch);
    }

    let entry_size = usize::try_from(header.entry_size).map_err(|_| StorageError::TooLarge)?;
    let mut partitions = Vec::new();
    for index in 0..usize::try_from(header.entry_count).map_err(|_| StorageError::TooLarge)? {
        let offset = index
            .checked_mul(entry_size)
            .ok_or(StorageError::TooLarge)?;
        let entry = &bytes[offset..offset + entry_size];
        let type_guid = Guid(
            entry[0..16]
                .try_into()
                .map_err(|_| StorageError::InvalidGpt)?,
        );
        if type_guid.is_zero() {
            continue;
        }
        let unique_guid = Guid(
            entry[16..32]
                .try_into()
                .map_err(|_| StorageError::InvalidGpt)?,
        );
        let first_lba = le_u64(entry, 32);
        let last_lba = le_u64(entry, 40);
        if unique_guid.is_zero()
            || first_lba < header.first_usable_lba
            || last_lba > header.last_usable_lba
            || first_lba > last_lba
        {
            return Err(StorageError::InvalidGpt);
        }
        let sector_count = last_lba
            .checked_sub(first_lba)
            .and_then(|value| value.checked_add(1))
            .ok_or(StorageError::InvalidGpt)?;
        partitions.push(Partition {
            index: u32::try_from(index + 1).map_err(|_| StorageError::TooLarge)?,
            first_lba,
            sector_count,
            bootable: false,
            mbr_type: None,
            type_guid: Some(type_guid),
            unique_guid: Some(unique_guid),
            attributes: le_u64(entry, 48),
            name: decode_gpt_name(&entry[56..entry_size.min(128)]),
        });
    }
    validate_no_overlap(&partitions)?;
    Ok(PartitionTable {
        kind: PartitionTableKind::Gpt,
        disk_guid: Some(header.disk_guid),
        partitions,
    })
}

#[cfg(feature = "alloc")]
fn decode_gpt_name(bytes: &[u8]) -> String {
    let mut output = String::new();
    for pair in bytes.as_chunks::<2>().0 {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        if value == 0 {
            break;
        }
        output.push(char::from_u32(u32::from(value)).unwrap_or('\u{fffd}'));
    }
    output
}

#[cfg(feature = "alloc")]
fn validate_partition_bounds(
    first_lba: u64,
    sector_count: u64,
    disk_sectors: u64,
) -> Result<(), StorageError> {
    if sector_count == 0
        || first_lba == 0
        || first_lba
            .checked_add(sector_count)
            .is_none_or(|end| end > disk_sectors)
    {
        return Err(StorageError::InvalidPartition);
    }
    Ok(())
}

#[cfg(feature = "alloc")]
fn validate_no_overlap(partitions: &[Partition]) -> Result<(), StorageError> {
    for (index, left) in partitions.iter().enumerate() {
        let left_end = left
            .first_lba
            .checked_add(left.sector_count)
            .ok_or(StorageError::InvalidPartition)?;
        for right in &partitions[index + 1..] {
            let right_end = right
                .first_lba
                .checked_add(right.sector_count)
                .ok_or(StorageError::InvalidPartition)?;
            if left.first_lba < right_end && right.first_lba < left_end {
                return Err(StorageError::InvalidPartition);
            }
        }
    }
    Ok(())
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    #[test]
    fn partition_device_enforces_its_window() {
        let mut disk = MemoryBlockDevice::new(16, 512).unwrap();
        {
            let mut partition = PartitionDevice::new(&mut disk, 4, 4).unwrap();
            partition.write_sectors(0, &[9; 512]).unwrap();
            assert_eq!(
                partition.write_sectors(4, &[1; 512]),
                Err(StorageError::OutOfBounds)
            );
        }
        let mut sector = [0; 512];
        disk.read_sectors(4, &mut sector).unwrap();
        assert_eq!(sector, [9; 512]);
    }

    #[test]
    fn reads_valid_mbr_partitions_and_rejects_overlap() {
        let mut disk = MemoryBlockDevice::new(32, 512).unwrap();
        let mut mbr = [0; 512];
        mbr[510..512].copy_from_slice(&MBR_SIGNATURE);
        write_mbr_entry(&mut mbr, 0, 0x80, 0x0c, 4, 8);
        disk.write_sectors(0, &mbr).unwrap();
        let table = read_partition_table(&mut disk).unwrap();
        assert_eq!(table.kind, PartitionTableKind::Mbr);
        assert_eq!(table.partitions[0].sector_count, 8);

        write_mbr_entry(&mut mbr, 1, 0, 0x83, 8, 4);
        disk.write_sectors(0, &mbr).unwrap();
        assert_eq!(
            read_partition_table(&mut disk),
            Err(StorageError::InvalidPartition)
        );
    }

    #[test]
    fn validates_gpt_entry_array_and_names() {
        let mut disk = MemoryBlockDevice::new(128, 512).unwrap();
        let mut protective_mbr = [0; 512];
        protective_mbr[510..512].copy_from_slice(&MBR_SIGNATURE);
        write_mbr_entry(&mut protective_mbr, 0, 0, 0xee, 1, 127);
        disk.write_sectors(0, &protective_mbr).unwrap();

        let mut entries = vec![0_u8; 32 * 512];
        entries[0..16].copy_from_slice(&[1; 16]);
        entries[16..32].copy_from_slice(&[2; 16]);
        entries[32..40].copy_from_slice(&40_u64.to_le_bytes());
        entries[40..48].copy_from_slice(&60_u64.to_le_bytes());
        for (index, character) in "NexOS root".encode_utf16().enumerate() {
            let offset = 56 + index * 2;
            entries[offset..offset + 2].copy_from_slice(&character.to_le_bytes());
        }
        let entries_crc = crc32(&entries);
        disk.write_sectors(2, &entries).unwrap();

        let mut header = [0_u8; 512];
        header[0..8].copy_from_slice(GPT_SIGNATURE);
        header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        header[12..16].copy_from_slice(&92_u32.to_le_bytes());
        header[24..32].copy_from_slice(&1_u64.to_le_bytes());
        header[32..40].copy_from_slice(&127_u64.to_le_bytes());
        header[40..48].copy_from_slice(&34_u64.to_le_bytes());
        header[48..56].copy_from_slice(&126_u64.to_le_bytes());
        header[56..72].copy_from_slice(&[3; 16]);
        header[72..80].copy_from_slice(&2_u64.to_le_bytes());
        header[80..84].copy_from_slice(&128_u32.to_le_bytes());
        header[84..88].copy_from_slice(&128_u32.to_le_bytes());
        header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let header_crc = crc32(&header[..92]);
        header[16..20].copy_from_slice(&header_crc.to_le_bytes());
        disk.write_sectors(1, &header).unwrap();

        let table = read_partition_table(&mut disk).unwrap();
        assert_eq!(table.kind, PartitionTableKind::Gpt);
        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].name, "NexOS root");
        assert_eq!(table.partitions[0].sector_count, 21);

        entries[0] ^= 1;
        disk.write_sectors(2, &entries).unwrap();
        assert_eq!(
            read_partition_table(&mut disk),
            Err(StorageError::ChecksumMismatch)
        );
    }

    fn write_mbr_entry(
        sector: &mut [u8],
        index: usize,
        status: u8,
        partition_type: u8,
        first_lba: u32,
        sectors: u32,
    ) {
        let offset = 446 + index * 16;
        sector[offset] = status;
        sector[offset + 4] = partition_type;
        sector[offset + 8..offset + 12].copy_from_slice(&first_lba.to_le_bytes());
        sector[offset + 12..offset + 16].copy_from_slice(&sectors.to_le_bytes());
    }
}
