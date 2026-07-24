use alloc::string::{String, ToString};
use core::str;

use nexos_storage::crc32;

use crate::{BLOCK_SIZE, DIR_ENTRY_SIZE, DIRECT_BLOCKS, FsError, INODE_SIZE, MAX_NAME_LENGTH};

const INODE_MAGIC: [u8; 4] = *b"NXIN";
const INODE_CHECKSUM_OFFSET: usize = INODE_SIZE - 4;
const DIRECTORY_CHECKSUM_OFFSET: usize = DIR_ENTRY_SIZE - 4;
const DIRECTORY_NAME_OFFSET: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FileType {
    Regular = 1,
    Directory = 2,
}

impl FileType {
    pub(crate) fn decode(value: u8) -> Result<Self, FsError> {
        match value {
            1 => Ok(Self::Regular),
            2 => Ok(Self::Directory),
            _ => Err(FsError::CorruptInode),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Inode {
    pub kind: FileType,
    pub permissions: u16,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub direct_blocks: [u64; DIRECT_BLOCKS],
    pub indirect_block: u64,
    pub block_count: u64,
    pub generation: u64,
}

impl Inode {
    pub(crate) const fn new(kind: FileType, permissions: u16, timestamp: u64) -> Self {
        Self {
            kind,
            permissions,
            size: 0,
            created: timestamp,
            modified: timestamp,
            direct_blocks: [0; DIRECT_BLOCKS],
            indirect_block: 0,
            block_count: 0,
            generation: timestamp,
        }
    }

    pub(crate) fn encode(&self, output: &mut [u8]) -> Result<(), FsError> {
        if output.len() < INODE_SIZE {
            return Err(FsError::UnsupportedBlockSize);
        }
        output[..INODE_SIZE].fill(0);
        output[0..4].copy_from_slice(&INODE_MAGIC);
        output[4] = self.kind as u8;
        put_u16(output, 6, self.permissions);
        put_u64(output, 8, self.size);
        put_u64(output, 16, self.created);
        put_u64(output, 24, self.modified);
        for (index, block) in self.direct_blocks.iter().enumerate() {
            put_u64(output, 32 + index * 8, *block);
        }
        put_u64(output, 128, self.indirect_block);
        put_u64(output, 136, self.block_count);
        put_u64(output, 144, self.generation);
        put_u32(output, INODE_CHECKSUM_OFFSET, 0);
        let checksum = crc32(&output[..INODE_SIZE]);
        put_u32(output, INODE_CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub(crate) fn decode(input: &[u8]) -> Result<Self, FsError> {
        if input.len() < INODE_SIZE || input[0..4] != INODE_MAGIC {
            return Err(FsError::CorruptInode);
        }
        let expected = get_u32(input, INODE_CHECKSUM_OFFSET);
        let mut encoded = [0_u8; INODE_SIZE];
        encoded.copy_from_slice(&input[..INODE_SIZE]);
        put_u32(&mut encoded, INODE_CHECKSUM_OFFSET, 0);
        if crc32(&encoded) != expected {
            return Err(FsError::ChecksumMismatch);
        }
        let mut direct_blocks = [0_u64; DIRECT_BLOCKS];
        for (index, block) in direct_blocks.iter_mut().enumerate() {
            *block = get_u64(input, 32 + index * 8);
        }
        Ok(Self {
            kind: FileType::decode(input[4])?,
            permissions: get_u16(input, 6),
            size: get_u64(input, 8),
            created: get_u64(input, 16),
            modified: get_u64(input, 24),
            direct_blocks,
            indirect_block: get_u64(input, 128),
            block_count: get_u64(input, 136),
            generation: get_u64(input, 144),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub inode: u64,
    pub kind: FileType,
    pub name: String,
}

impl DirectoryEntry {
    pub(crate) fn new(inode: u64, kind: FileType, name: &str) -> Result<Self, FsError> {
        validate_name(name)?;
        Ok(Self {
            inode,
            kind,
            name: name.to_string(),
        })
    }

    pub(crate) fn encode(&self, output: &mut [u8]) -> Result<(), FsError> {
        validate_name(&self.name)?;
        if output.len() < DIR_ENTRY_SIZE {
            return Err(FsError::UnsupportedBlockSize);
        }
        output[..DIR_ENTRY_SIZE].fill(0);
        put_u64(output, 0, self.inode);
        output[8] = self.kind as u8;
        output[9] = u8::try_from(self.name.len()).map_err(|_| FsError::InvalidName)?;
        let end = DIRECTORY_NAME_OFFSET + self.name.len();
        output[DIRECTORY_NAME_OFFSET..end].copy_from_slice(self.name.as_bytes());
        put_u32(output, DIRECTORY_CHECKSUM_OFFSET, 0);
        let checksum = crc32(&output[..DIR_ENTRY_SIZE]);
        put_u32(output, DIRECTORY_CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub(crate) fn decode(input: &[u8]) -> Result<Self, FsError> {
        if input.len() < DIR_ENTRY_SIZE {
            return Err(FsError::CorruptDirectory);
        }
        let expected = get_u32(input, DIRECTORY_CHECKSUM_OFFSET);
        let mut encoded = [0_u8; DIR_ENTRY_SIZE];
        encoded.copy_from_slice(&input[..DIR_ENTRY_SIZE]);
        put_u32(&mut encoded, DIRECTORY_CHECKSUM_OFFSET, 0);
        if crc32(&encoded) != expected {
            return Err(FsError::ChecksumMismatch);
        }
        let inode = get_u64(input, 0);
        if inode == 0 {
            return Err(FsError::CorruptDirectory);
        }
        let length = usize::from(input[9]);
        if length == 0 || length > MAX_NAME_LENGTH {
            return Err(FsError::InvalidName);
        }
        let end = DIRECTORY_NAME_OFFSET
            .checked_add(length)
            .ok_or(FsError::CorruptDirectory)?;
        let name =
            str::from_utf8(&input[DIRECTORY_NAME_OFFSET..end]).map_err(|_| FsError::InvalidName)?;
        validate_name(name)?;
        Ok(Self {
            inode,
            kind: FileType::decode(input[8]).map_err(|_| FsError::CorruptDirectory)?,
            name: name.to_string(),
        })
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), FsError> {
    if name.is_empty()
        || name.len() > MAX_NAME_LENGTH
        || name == "."
        || name == ".."
        || name.bytes().any(|byte| byte == b'/' || byte == 0)
    {
        return Err(FsError::InvalidName);
    }
    Ok(())
}

pub(crate) const fn data_blocks_for_size(size: u64) -> u64 {
    size.div_ceil(BLOCK_SIZE as u64)
}

pub(crate) fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

pub(crate) fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub(crate) fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
