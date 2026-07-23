#![no_std]
#![allow(clippy::missing_errors_doc)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

pub const MBR_SIGNATURE: [u8; 2] = [0x55, 0xaa];
pub const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageError {
    OutOfBounds,
    InvalidBuffer,
    ReadOnly,
    InvalidMbr,
    InvalidGpt,
    ChecksumMismatch,
    UnsupportedSectorSize,
    Device,
}

pub trait BlockDevice {
    fn sector_size(&self) -> u32;
    fn sector_count(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError>;
    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError>;
    fn flush(&mut self) -> Result<(), StorageError>;
}

#[derive(Clone, Debug)]
pub struct MemoryBlockDevice {
    bytes: Vec<u8>,
    sector_size: u32,
    read_only: bool,
}

impl MemoryBlockDevice {
    pub fn new(sectors: u64, sector_size: u32) -> Result<Self, StorageError> {
        if sector_size < 512 || !sector_size.is_power_of_two() {
            return Err(StorageError::UnsupportedSectorSize);
        }
        let length = sectors
            .checked_mul(u64::from(sector_size))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(StorageError::OutOfBounds)?;
        Ok(Self {
            bytes: vec![0; length],
            sector_size,
            read_only: false,
        })
    }

    pub fn from_bytes(bytes: Vec<u8>, sector_size: u32) -> Result<Self, StorageError> {
        if sector_size < 512
            || !sector_size.is_power_of_two()
            || !bytes.len().is_multiple_of(sector_size as usize)
        {
            return Err(StorageError::InvalidBuffer);
        }
        Ok(Self {
            bytes,
            sector_size,
            read_only: false,
        })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    fn range(&self, lba: u64, length: usize) -> Result<core::ops::Range<usize>, StorageError> {
        if !length.is_multiple_of(self.sector_size as usize) {
            return Err(StorageError::InvalidBuffer);
        }
        let start = lba
            .checked_mul(u64::from(self.sector_size))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(StorageError::OutOfBounds)?;
        let end = start.checked_add(length).ok_or(StorageError::OutOfBounds)?;
        if end > self.bytes.len() {
            return Err(StorageError::OutOfBounds);
        }
        Ok(start..end)
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.bytes.len() as u64 / u64::from(self.sector_size)
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let range = self.range(lba, output.len())?;
        output.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        if self.read_only {
            return Err(StorageError::ReadOnly);
        }
        let range = self.range(lba, input.len())?;
        self.bytes[range].copy_from_slice(input);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MbrPartition {
    pub bootable: bool,
    pub partition_type: u8,
    pub first_lba: u32,
    pub sector_count: u32,
}

pub fn parse_mbr(sector: &[u8]) -> Result<[Option<MbrPartition>; 4], StorageError> {
    if sector.len() < 512 || sector[510..512] != MBR_SIGNATURE {
        return Err(StorageError::InvalidMbr);
    }
    let mut result = [None; 4];
    for (index, slot) in result.iter_mut().enumerate() {
        let offset = 446 + index * 16;
        let entry = &sector[offset..offset + 16];
        let partition_type = entry[4];
        let first_lba = le_u32(entry, 8);
        let sector_count = le_u32(entry, 12);
        if partition_type != 0 && sector_count != 0 {
            *slot = Some(MbrPartition {
                bootable: entry[0] == 0x80,
                partition_type,
                first_lba,
                sector_count,
            });
        }
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GptHeader {
    pub revision: u32,
    pub header_size: u32,
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc32: u32,
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
    let mut header = sector[..header_size as usize].to_vec();
    header[16..20].fill(0);
    if crc32(&header) != expected_crc {
        return Err(StorageError::ChecksumMismatch);
    }
    let parsed = GptHeader {
        revision: le_u32(sector, 8),
        header_size,
        current_lba: le_u64(sector, 24),
        backup_lba: le_u64(sector, 32),
        first_usable_lba: le_u64(sector, 40),
        last_usable_lba: le_u64(sector, 48),
        entries_lba: le_u64(sector, 72),
        entry_count: le_u32(sector, 80),
        entry_size: le_u32(sector, 84),
        entries_crc32: le_u32(sector, 88),
    };
    if parsed.revision != 0x0001_0000
        || parsed.current_lba == parsed.backup_lba
        || parsed.first_usable_lba > parsed.last_usable_lba
        || parsed.entry_size < 128
        || !parsed.entry_size.is_multiple_of(8)
    {
        return Err(StorageError::InvalidGpt);
    }
    Ok(parsed)
}

#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
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

    #[test]
    fn memory_device_checks_alignment_and_bounds() {
        let mut disk = MemoryBlockDevice::new(4, 512).unwrap();
        assert_eq!(
            disk.write_sectors(0, &[1; 3]),
            Err(StorageError::InvalidBuffer)
        );
        assert_eq!(
            disk.read_sectors(4, &mut [0; 512]),
            Err(StorageError::OutOfBounds)
        );
        disk.write_sectors(2, &[7; 512]).unwrap();
        let mut output = [0; 512];
        disk.read_sectors(2, &mut output).unwrap();
        assert_eq!(output, [7; 512]);
    }

    #[test]
    fn parses_a_valid_mbr() {
        let mut sector = [0_u8; 512];
        sector[510..512].copy_from_slice(&MBR_SIGNATURE);
        sector[446] = 0x80;
        sector[450] = 0x83;
        sector[454..458].copy_from_slice(&2048_u32.to_le_bytes());
        sector[458..462].copy_from_slice(&4096_u32.to_le_bytes());
        let partitions = parse_mbr(&sector).unwrap();
        assert_eq!(partitions[0].unwrap().first_lba, 2048);
        assert_eq!(partitions[0].unwrap().partition_type, 0x83);
    }

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn validates_gpt_header_crc() {
        let mut sector = [0_u8; 512];
        sector[0..8].copy_from_slice(GPT_SIGNATURE);
        sector[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        sector[12..16].copy_from_slice(&92_u32.to_le_bytes());
        sector[24..32].copy_from_slice(&1_u64.to_le_bytes());
        sector[32..40].copy_from_slice(&999_u64.to_le_bytes());
        sector[40..48].copy_from_slice(&34_u64.to_le_bytes());
        sector[48..56].copy_from_slice(&966_u64.to_le_bytes());
        sector[72..80].copy_from_slice(&2_u64.to_le_bytes());
        sector[80..84].copy_from_slice(&128_u32.to_le_bytes());
        sector[84..88].copy_from_slice(&128_u32.to_le_bytes());
        let checksum = crc32(&sector[..92]);
        sector[16..20].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(parse_gpt_header(&sector).unwrap().backup_lba, 999);
        sector[40] ^= 1;
        assert_eq!(
            parse_gpt_header(&sector),
            Err(StorageError::ChecksumMismatch)
        );
    }
}
