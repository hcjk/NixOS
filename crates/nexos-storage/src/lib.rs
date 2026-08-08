#![no_std]
#![allow(clippy::missing_errors_doc)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(feature = "alloc")]
mod cache;
#[cfg(feature = "alloc")]
mod fat32;
mod installer;
mod partition;

#[cfg(feature = "alloc")]
pub use cache::{CacheStats, CachedBlockDevice};
#[cfg(feature = "alloc")]
pub use fat32::{Fat32, Fat32Info, FatDirectoryEntry, format_fat32};
pub use installer::{
    BIOS_BOOT_TYPE_GUID, ESP_TYPE_GUID, InstallerGuids, InstallerLayout, NEXFS_TYPE_GUID,
    PartitionSpan, guided_installer_layout, guided_installer_layout_for_sector_size,
    verify_guided_installer_gpt, write_guided_installer_gpt,
};
pub use partition::{GptHeader, Guid, MbrPartition, PartitionDevice, parse_gpt_header, parse_mbr};
#[cfg(feature = "alloc")]
pub use partition::{Partition, PartitionTable, PartitionTableKind, read_partition_table};

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
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
    InvalidPartition,
    ChecksumMismatch,
    UnsupportedSectorSize,
    UnsupportedFilesystem,
    CorruptFilesystem,
    NotFound,
    AlreadyExists,
    InvalidName,
    NoSpace,
    TooLarge,
    Timeout,
    Busy,
    Device,
}

pub trait BlockDevice {
    fn sector_size(&self) -> u32;
    fn sector_count(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError>;
    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError>;
    fn flush(&mut self) -> Result<(), StorageError>;
}

#[cfg(feature = "alloc")]
#[derive(Clone, Debug)]
pub struct MemoryBlockDevice {
    bytes: Vec<u8>,
    sector_size: u32,
    read_only: bool,
}

#[cfg(feature = "alloc")]
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

#[cfg(feature = "alloc")]
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
