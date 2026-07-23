#![no_std]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

extern crate alloc;

use alloc::vec;
use nexos_storage::{BlockDevice, StorageError, crc32};

pub const MAGIC: [u8; 8] = *b"NEXFS\x01\0\0";
pub const VERSION: u32 = 1;
pub const BLOCK_SIZE: u32 = 4096;
pub const SUPERBLOCK_BLOCK: u64 = 1;
pub const CLEAN_FLAG: u32 = 1;
const MIN_BLOCKS: u64 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsError {
    Storage(StorageError),
    TooSmall,
    UnsupportedBlockSize,
    InvalidMagic,
    UnsupportedVersion,
    InvalidLayout,
    ChecksumMismatch,
    Dirty,
}

impl From<StorageError> for FsError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Superblock {
    pub version: u32,
    pub block_size: u32,
    pub total_blocks: u64,
    pub inode_bitmap_block: u64,
    pub block_bitmap_block: u64,
    pub inode_table_block: u64,
    pub data_start_block: u64,
    pub root_inode: u64,
    pub flags: u32,
    pub generation: u64,
    pub uuid: [u8; 16],
}

impl Superblock {
    pub fn encode(&self, output: &mut [u8]) -> Result<(), FsError> {
        if output.len() < BLOCK_SIZE as usize {
            return Err(FsError::UnsupportedBlockSize);
        }
        output.fill(0);
        output[0..8].copy_from_slice(&MAGIC);
        put_u32(output, 8, self.version);
        put_u32(output, 12, self.block_size);
        put_u64(output, 16, self.total_blocks);
        put_u64(output, 24, self.inode_bitmap_block);
        put_u64(output, 32, self.block_bitmap_block);
        put_u64(output, 40, self.inode_table_block);
        put_u64(output, 48, self.data_start_block);
        put_u64(output, 56, self.root_inode);
        put_u32(output, 64, self.flags);
        put_u64(output, 72, self.generation);
        output[80..96].copy_from_slice(&self.uuid);
        put_u32(output, 96, 0);
        let checksum = crc32(&output[..100]);
        put_u32(output, 96, checksum);
        Ok(())
    }

    pub fn decode(input: &[u8]) -> Result<Self, FsError> {
        if input.len() < BLOCK_SIZE as usize {
            return Err(FsError::UnsupportedBlockSize);
        }
        if input[0..8] != MAGIC {
            return Err(FsError::InvalidMagic);
        }
        let expected = get_u32(input, 96);
        let mut checksum_data = input[..100].to_vec();
        put_u32(&mut checksum_data, 96, 0);
        if crc32(&checksum_data) != expected {
            return Err(FsError::ChecksumMismatch);
        }
        let result = Self {
            version: get_u32(input, 8),
            block_size: get_u32(input, 12),
            total_blocks: get_u64(input, 16),
            inode_bitmap_block: get_u64(input, 24),
            block_bitmap_block: get_u64(input, 32),
            inode_table_block: get_u64(input, 40),
            data_start_block: get_u64(input, 48),
            root_inode: get_u64(input, 56),
            flags: get_u32(input, 64),
            generation: get_u64(input, 72),
            uuid: input[80..96].try_into().unwrap(),
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), FsError> {
        if self.version != VERSION {
            return Err(FsError::UnsupportedVersion);
        }
        if self.block_size != BLOCK_SIZE {
            return Err(FsError::UnsupportedBlockSize);
        }
        if self.total_blocks < MIN_BLOCKS
            || self.inode_bitmap_block != 2
            || self.block_bitmap_block <= self.inode_bitmap_block
            || self.inode_table_block <= self.block_bitmap_block
            || self.data_start_block <= self.inode_table_block
            || self.data_start_block >= self.total_blocks
            || self.root_inode != 1
        {
            return Err(FsError::InvalidLayout);
        }
        Ok(())
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.flags & CLEAN_FLAG != 0
    }
}

pub fn format<D: BlockDevice>(device: &mut D, uuid: [u8; 16]) -> Result<Superblock, FsError> {
    if !BLOCK_SIZE.is_multiple_of(device.sector_size()) {
        return Err(FsError::UnsupportedBlockSize);
    }
    let bytes = device
        .sector_count()
        .checked_mul(u64::from(device.sector_size()))
        .ok_or(FsError::TooSmall)?;
    let total_blocks = bytes / u64::from(BLOCK_SIZE);
    if total_blocks < MIN_BLOCKS {
        return Err(FsError::TooSmall);
    }
    let block_bitmap_blocks = total_blocks.div_ceil(u64::from(BLOCK_SIZE) * 8);
    let inode_bitmap_block = 2;
    let block_bitmap_block = 3;
    let inode_table_block = block_bitmap_block + block_bitmap_blocks;
    let inode_table_blocks = core::cmp::max(4, total_blocks / 256);
    let data_start_block = inode_table_block + inode_table_blocks;
    if data_start_block >= total_blocks {
        return Err(FsError::TooSmall);
    }
    let superblock = Superblock {
        version: VERSION,
        block_size: BLOCK_SIZE,
        total_blocks,
        inode_bitmap_block,
        block_bitmap_block,
        inode_table_block,
        data_start_block,
        root_inode: 1,
        flags: CLEAN_FLAG,
        generation: 1,
        uuid,
    };

    let sectors_per_block = u64::from(BLOCK_SIZE / device.sector_size());
    let mut block = vec![0_u8; BLOCK_SIZE as usize];
    superblock.encode(&mut block)?;
    device.write_sectors(SUPERBLOCK_BLOCK * sectors_per_block, &block)?;

    // Reserve metadata blocks and the root inode in the initial bitmaps.
    block.fill(0);
    block[0] = 0b0000_0011;
    device.write_sectors(inode_bitmap_block * sectors_per_block, &block)?;
    block.fill(0);
    for index in 0..data_start_block {
        let byte_index = usize::try_from(index).map_err(|_| FsError::InvalidLayout)? / 8;
        block[byte_index] |= 1 << (index % 8);
    }
    device.write_sectors(block_bitmap_block * sectors_per_block, &block)?;
    device.flush()?;
    Ok(superblock)
}

pub fn check<D: BlockDevice>(device: &mut D) -> Result<Superblock, FsError> {
    if !BLOCK_SIZE.is_multiple_of(device.sector_size()) {
        return Err(FsError::UnsupportedBlockSize);
    }
    let sectors_per_block = u64::from(BLOCK_SIZE / device.sector_size());
    let mut block = vec![0_u8; BLOCK_SIZE as usize];
    device.read_sectors(SUPERBLOCK_BLOCK * sectors_per_block, &mut block)?;
    let superblock = Superblock::decode(&block)?;
    let available_blocks = device
        .sector_count()
        .checked_mul(u64::from(device.sector_size()))
        .ok_or(FsError::InvalidLayout)?
        / u64::from(BLOCK_SIZE);
    if superblock.total_blocks > available_blocks {
        return Err(FsError::InvalidLayout);
    }
    if !superblock.is_clean() {
        return Err(FsError::Dirty);
    }
    Ok(superblock)
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexos_storage::MemoryBlockDevice;

    #[test]
    fn formats_and_checks_a_volume() {
        let mut disk = MemoryBlockDevice::new(4096, 512).unwrap();
        let uuid = [0x42; 16];
        let formatted = format(&mut disk, uuid).unwrap();
        let checked = check(&mut disk).unwrap();
        assert_eq!(formatted, checked);
        assert_eq!(checked.uuid, uuid);
        assert!(checked.is_clean());
    }

    #[test]
    fn rejects_corrupt_superblock() {
        let mut disk = MemoryBlockDevice::new(4096, 512).unwrap();
        format(&mut disk, [1; 16]).unwrap();
        let mut block = vec![0_u8; BLOCK_SIZE as usize];
        let superblock_lba = SUPERBLOCK_BLOCK * u64::from(BLOCK_SIZE / 512);
        disk.read_sectors(superblock_lba, &mut block).unwrap();
        block[40] ^= 1;
        disk.write_sectors(superblock_lba, &block).unwrap();
        assert_eq!(check(&mut disk), Err(FsError::ChecksumMismatch));
    }

    #[test]
    fn rejects_tiny_volume() {
        let mut disk = MemoryBlockDevice::new(16, 512).unwrap();
        assert_eq!(format(&mut disk, [0; 16]), Err(FsError::TooSmall));
    }
}
