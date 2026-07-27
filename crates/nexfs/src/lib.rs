#![no_std]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

use nexos_storage::{BlockDevice, StorageError, crc32};

mod layout;
mod volume;

pub use layout::{DirectoryEntry, FileType, Inode};
pub use volume::{FileStat, NexFs};

pub const MAGIC: [u8; 8] = *b"NEXFS\x01\0\0";
pub const VERSION: u32 = 1;
pub const BLOCK_SIZE: usize = 4096;
pub const SUPERBLOCK_BLOCK: u64 = 1;
pub const CLEAN_FLAG: u32 = 1;
pub const INODE_SIZE: usize = 256;
pub const DIRECT_BLOCKS: usize = 12;
pub const DIR_ENTRY_SIZE: usize = 256;
pub const MAX_NAME_LENGTH: usize = 240;

const MIN_BLOCKS: u64 = 32;
const BLOCK_SIZE_U32: u32 = 4096;
const BLOCK_SIZE_U64: u64 = 4096;
const INODES_PER_BLOCK: u64 = (BLOCK_SIZE / INODE_SIZE) as u64;
const BITMAP_BITS_PER_BLOCK: u64 = (BLOCK_SIZE * 8) as u64;
const MAX_INODE_TABLE_BLOCKS: u64 = BITMAP_BITS_PER_BLOCK / INODES_PER_BLOCK;
const INDIRECT_POINTERS: usize = BLOCK_SIZE / 8;
const MAX_FILE_BLOCKS: usize = DIRECT_BLOCKS + INDIRECT_POINTERS;

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
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    DirectoryNotEmpty,
    InvalidName,
    InvalidPath,
    NoSpace,
    FileTooLarge,
    TooLarge,
    CorruptInode,
    CorruptDirectory,
    Busy,
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
        if output.len() < BLOCK_SIZE {
            return Err(FsError::UnsupportedBlockSize);
        }
        output[..BLOCK_SIZE].fill(0);
        output[0..8].copy_from_slice(&MAGIC);
        layout::put_u32(output, 8, self.version);
        layout::put_u32(output, 12, self.block_size);
        layout::put_u64(output, 16, self.total_blocks);
        layout::put_u64(output, 24, self.inode_bitmap_block);
        layout::put_u64(output, 32, self.block_bitmap_block);
        layout::put_u64(output, 40, self.inode_table_block);
        layout::put_u64(output, 48, self.data_start_block);
        layout::put_u64(output, 56, self.root_inode);
        layout::put_u32(output, 64, self.flags);
        layout::put_u64(output, 72, self.generation);
        output[80..96].copy_from_slice(&self.uuid);
        layout::put_u32(output, 96, 0);
        let checksum = crc32(&output[..100]);
        layout::put_u32(output, 96, checksum);
        Ok(())
    }

    pub fn decode(input: &[u8]) -> Result<Self, FsError> {
        if input.len() < BLOCK_SIZE {
            return Err(FsError::UnsupportedBlockSize);
        }
        if input[0..8] != MAGIC {
            return Err(FsError::InvalidMagic);
        }
        let expected = layout::get_u32(input, 96);
        let mut checksum_data = input[..100].to_vec();
        layout::put_u32(&mut checksum_data, 96, 0);
        if crc32(&checksum_data) != expected {
            return Err(FsError::ChecksumMismatch);
        }
        let result = Self {
            version: layout::get_u32(input, 8),
            block_size: layout::get_u32(input, 12),
            total_blocks: layout::get_u64(input, 16),
            inode_bitmap_block: layout::get_u64(input, 24),
            block_bitmap_block: layout::get_u64(input, 32),
            inode_table_block: layout::get_u64(input, 40),
            data_start_block: layout::get_u64(input, 48),
            root_inode: layout::get_u64(input, 56),
            flags: layout::get_u32(input, 64),
            generation: layout::get_u64(input, 72),
            uuid: input[80..96].try_into().unwrap(),
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), FsError> {
        if self.version != VERSION {
            return Err(FsError::UnsupportedVersion);
        }
        if self.block_size != BLOCK_SIZE_U32 {
            return Err(FsError::UnsupportedBlockSize);
        }
        let block_bitmap_blocks = self.block_bitmap_blocks();
        let inode_table_blocks = self
            .data_start_block
            .checked_sub(self.inode_table_block)
            .ok_or(FsError::InvalidLayout)?;
        if self.total_blocks < MIN_BLOCKS
            || self.inode_bitmap_block != 2
            || self.block_bitmap_block != 3
            || self.inode_table_block != self.block_bitmap_block + block_bitmap_blocks
            || inode_table_blocks == 0
            || inode_table_blocks > MAX_INODE_TABLE_BLOCKS
            || self.data_start_block >= self.total_blocks
            || self.root_inode != 1
        {
            return Err(FsError::InvalidLayout);
        }
        Ok(())
    }

    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.flags & CLEAN_FLAG != 0
    }

    #[must_use]
    pub const fn block_bitmap_blocks(&self) -> u64 {
        self.total_blocks.div_ceil(BITMAP_BITS_PER_BLOCK)
    }

    pub fn inode_count(&self) -> Result<u64, FsError> {
        self.data_start_block
            .checked_sub(self.inode_table_block)
            .and_then(|blocks| blocks.checked_mul(INODES_PER_BLOCK))
            .filter(|count| *count > self.root_inode && *count <= BITMAP_BITS_PER_BLOCK)
            .ok_or(FsError::InvalidLayout)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckReport {
    pub superblock: Superblock,
    pub allocated_inodes: u64,
    pub allocated_blocks: u64,
    pub files: u64,
    pub directories: u64,
    pub directory_entries: u64,
}

pub fn format<D: BlockDevice>(device: &mut D, uuid: [u8; 16]) -> Result<Superblock, FsError> {
    let sectors_per_block = sectors_per_block(device)?;
    let total_blocks = device_bytes(device)? / BLOCK_SIZE_U64;
    if total_blocks < MIN_BLOCKS {
        return Err(FsError::TooSmall);
    }
    let block_bitmap_blocks = total_blocks.div_ceil(BITMAP_BITS_PER_BLOCK);
    let inode_bitmap_block = 2;
    let block_bitmap_block = 3;
    let inode_table_block = block_bitmap_block + block_bitmap_blocks;
    let inode_table_blocks = (total_blocks / 256).clamp(4, MAX_INODE_TABLE_BLOCKS);
    let data_start_block = inode_table_block + inode_table_blocks;
    if data_start_block >= total_blocks {
        return Err(FsError::TooSmall);
    }
    let superblock = Superblock {
        version: VERSION,
        block_size: BLOCK_SIZE_U32,
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
    superblock.validate()?;

    let mut dirty_superblock = superblock;
    dirty_superblock.flags &= !CLEAN_FLAG;
    write_superblock(device, &dirty_superblock)?;
    device.flush()?;

    let zero = vec![0_u8; BLOCK_SIZE];
    for block in inode_bitmap_block..data_start_block {
        device.write_sectors(block * sectors_per_block, &zero)?;
    }

    let mut inode_bitmap = vec![0_u8; BLOCK_SIZE];
    set_bitmap_bit(&mut inode_bitmap, 0, true)?;
    set_bitmap_bit(&mut inode_bitmap, superblock.root_inode, true)?;
    device.write_sectors(inode_bitmap_block * sectors_per_block, &inode_bitmap)?;

    let mut block_bitmap = vec![0_u8; BLOCK_SIZE];
    for index in 0..block_bitmap_blocks {
        block_bitmap.fill(0);
        let first_bit = index
            .checked_mul(BITMAP_BITS_PER_BLOCK)
            .ok_or(FsError::TooLarge)?;
        let reserved_end = data_start_block.min(
            first_bit
                .checked_add(BITMAP_BITS_PER_BLOCK)
                .ok_or(FsError::TooLarge)?,
        );
        for block in first_bit..reserved_end {
            set_bitmap_bit(&mut block_bitmap, block - first_bit, true)?;
        }
        device.write_sectors(
            (block_bitmap_block + index) * sectors_per_block,
            &block_bitmap,
        )?;
    }

    let root = Inode::new(FileType::Directory, 0o755, superblock.generation);
    let mut inode_block = vec![0_u8; BLOCK_SIZE];
    let root_offset = INODE_SIZE;
    root.encode(&mut inode_block[root_offset..root_offset + INODE_SIZE])?;
    device.write_sectors(inode_table_block * sectors_per_block, &inode_block)?;

    write_superblock(device, &superblock)?;
    device.flush()?;
    Ok(superblock)
}

pub fn check<D: BlockDevice>(device: &mut D) -> Result<Superblock, FsError> {
    Ok(check_detailed(device)?.superblock)
}

pub fn inspect_superblock<D: BlockDevice>(device: &mut D) -> Result<Superblock, FsError> {
    let superblock = read_superblock(device)?;
    if !superblock.is_clean() {
        return Err(FsError::Dirty);
    }
    let available_blocks = device_bytes(device)? / BLOCK_SIZE_U64;
    if superblock.total_blocks > available_blocks {
        return Err(FsError::InvalidLayout);
    }
    let mut inode_bitmap = vec![0_u8; BLOCK_SIZE];
    read_block(
        device,
        &superblock,
        superblock.inode_bitmap_block,
        &mut inode_bitmap,
    )?;
    if !bitmap_bit(&inode_bitmap, 0)? || !bitmap_bit(&inode_bitmap, superblock.root_inode)? {
        return Err(FsError::InvalidLayout);
    }
    let root = read_inode(device, &superblock, superblock.root_inode)?;
    if root.kind != FileType::Directory {
        return Err(FsError::InvalidLayout);
    }
    Ok(superblock)
}

#[allow(clippy::too_many_lines)]
pub fn check_detailed<D: BlockDevice>(device: &mut D) -> Result<CheckReport, FsError> {
    let superblock = read_superblock(device)?;
    if !superblock.is_clean() {
        return Err(FsError::Dirty);
    }
    let available_blocks = device_bytes(device)? / BLOCK_SIZE_U64;
    if superblock.total_blocks > available_blocks {
        return Err(FsError::InvalidLayout);
    }
    let inode_count = superblock.inode_count()?;
    let inode_count_usize = usize::try_from(inode_count).map_err(|_| FsError::TooLarge)?;
    let total_blocks_usize =
        usize::try_from(superblock.total_blocks).map_err(|_| FsError::TooLarge)?;

    let mut inode_bitmap = vec![0_u8; BLOCK_SIZE];
    read_block(
        device,
        &superblock,
        superblock.inode_bitmap_block,
        &mut inode_bitmap,
    )?;
    if !bitmap_bit(&inode_bitmap, 0)? || !bitmap_bit(&inode_bitmap, superblock.root_inode)? {
        return Err(FsError::InvalidLayout);
    }

    let bitmap_length = usize::try_from(superblock.block_bitmap_blocks())
        .ok()
        .and_then(|blocks| blocks.checked_mul(BLOCK_SIZE))
        .ok_or(FsError::TooLarge)?;
    let mut block_bitmap = vec![0_u8; bitmap_length];
    for index in 0..superblock.block_bitmap_blocks() {
        let start = usize::try_from(index)
            .map_err(|_| FsError::TooLarge)?
            .checked_mul(BLOCK_SIZE)
            .ok_or(FsError::TooLarge)?;
        read_block(
            device,
            &superblock,
            superblock.block_bitmap_block + index,
            &mut block_bitmap[start..start + BLOCK_SIZE],
        )?;
    }

    let mut owners = vec![0_u8; total_blocks_usize];
    for block in 0..superblock.data_start_block {
        let index = usize::try_from(block).map_err(|_| FsError::TooLarge)?;
        if !bitmap_bit(&block_bitmap, block)? || owners[index] != 0 {
            return Err(FsError::InvalidLayout);
        }
        owners[index] = 1;
    }

    let mut inodes = vec![None; inode_count_usize];
    let mut allocated_inodes = 0_u64;
    let mut files = 0_u64;
    let mut directories = 0_u64;
    for inode_number in 1..inode_count {
        if !bitmap_bit(&inode_bitmap, inode_number)? {
            continue;
        }
        allocated_inodes += 1;
        let inode = read_inode(device, &superblock, inode_number)?;
        if inode_number == superblock.root_inode && inode.kind != FileType::Directory {
            return Err(FsError::InvalidLayout);
        }
        match inode.kind {
            FileType::Regular => files += 1,
            FileType::Directory => {
                directories += 1;
                if !inode.size.is_multiple_of(DIR_ENTRY_SIZE as u64) {
                    return Err(FsError::CorruptDirectory);
                }
            }
        }
        validate_inode_blocks(device, &superblock, &block_bitmap, &mut owners, &inode)?;
        inodes[usize::try_from(inode_number).map_err(|_| FsError::TooLarge)?] = Some(inode);
    }

    for block in superblock.data_start_block..superblock.total_blocks {
        let allocated = bitmap_bit(&block_bitmap, block)?;
        let owned = owners[usize::try_from(block).map_err(|_| FsError::TooLarge)?] == 2;
        if allocated != owned {
            return Err(FsError::InvalidLayout);
        }
    }
    for block in superblock.total_blocks..(block_bitmap.len() as u64 * 8) {
        if bitmap_bit(&block_bitmap, block)? {
            return Err(FsError::InvalidLayout);
        }
    }
    for inode_number in inode_count..BITMAP_BITS_PER_BLOCK {
        if bitmap_bit(&inode_bitmap, inode_number)? {
            return Err(FsError::InvalidLayout);
        }
    }

    let mut reached = vec![false; inode_count_usize];
    reached[usize::try_from(superblock.root_inode).map_err(|_| FsError::TooLarge)?] = true;
    let mut pending = vec![superblock.root_inode];
    let mut directory_entries = 0_u64;
    while let Some(directory_number) = pending.pop() {
        let directory = inodes
            .get(usize::try_from(directory_number).map_err(|_| FsError::TooLarge)?)
            .and_then(|inode| *inode)
            .ok_or(FsError::CorruptDirectory)?;
        if directory.kind != FileType::Directory {
            return Err(FsError::NotDirectory);
        }
        let contents = read_all_inode_data(device, &superblock, &directory)?;
        let mut names = BTreeSet::new();
        let (encoded_entries, remainder) = contents.as_chunks::<DIR_ENTRY_SIZE>();
        if !remainder.is_empty() {
            return Err(FsError::CorruptDirectory);
        }
        for encoded in encoded_entries {
            let entry = DirectoryEntry::decode(encoded)?;
            directory_entries += 1;
            if !names.insert(entry.name.clone()) {
                return Err(FsError::CorruptDirectory);
            }
            if entry.inode == superblock.root_inode || entry.inode >= inode_count {
                return Err(FsError::CorruptDirectory);
            }
            let target_index = usize::try_from(entry.inode).map_err(|_| FsError::TooLarge)?;
            let target = inodes
                .get(target_index)
                .and_then(|inode| *inode)
                .ok_or(FsError::CorruptDirectory)?;
            if target.kind != entry.kind || reached[target_index] {
                return Err(FsError::CorruptDirectory);
            }
            reached[target_index] = true;
            if target.kind == FileType::Directory {
                pending.push(entry.inode);
            }
        }
    }
    for inode_number in 1..inode_count {
        let index = usize::try_from(inode_number).map_err(|_| FsError::TooLarge)?;
        if inode_bitmap.get(index / 8).is_some()
            && bitmap_bit(&inode_bitmap, inode_number)?
            && !reached[index]
        {
            return Err(FsError::InvalidLayout);
        }
    }

    let allocated_blocks = block_bitmap
        .iter()
        .map(|byte| u64::from(byte.count_ones()))
        .sum();
    Ok(CheckReport {
        superblock,
        allocated_inodes,
        allocated_blocks,
        files,
        directories,
        directory_entries,
    })
}

pub(crate) fn read_superblock<D: BlockDevice>(device: &mut D) -> Result<Superblock, FsError> {
    let sectors_per_block = sectors_per_block(device)?;
    let mut block = vec![0_u8; BLOCK_SIZE];
    device.read_sectors(SUPERBLOCK_BLOCK * sectors_per_block, &mut block)?;
    Superblock::decode(&block)
}

pub(crate) fn write_superblock<D: BlockDevice>(
    device: &mut D,
    superblock: &Superblock,
) -> Result<(), FsError> {
    let sectors_per_block = sectors_per_block(device)?;
    let mut block = vec![0_u8; BLOCK_SIZE];
    superblock.encode(&mut block)?;
    device.write_sectors(SUPERBLOCK_BLOCK * sectors_per_block, &block)?;
    Ok(())
}

fn validate_inode_blocks<D: BlockDevice>(
    device: &mut D,
    superblock: &Superblock,
    block_bitmap: &[u8],
    owners: &mut [u8],
    inode: &Inode,
) -> Result<(), FsError> {
    let expected = layout::data_blocks_for_size(inode.size);
    if expected != inode.block_count || expected > MAX_FILE_BLOCKS as u64 {
        return Err(FsError::CorruptInode);
    }
    let direct_count = core::cmp::min(expected, DIRECT_BLOCKS as u64);
    for index in 0..DIRECT_BLOCKS {
        let pointer = inode.direct_blocks[index];
        if (index as u64) < direct_count {
            claim_data_block(superblock, block_bitmap, owners, pointer)?;
        } else if pointer != 0 {
            return Err(FsError::CorruptInode);
        }
    }
    if expected <= DIRECT_BLOCKS as u64 {
        if inode.indirect_block != 0 {
            return Err(FsError::CorruptInode);
        }
        return Ok(());
    }
    claim_data_block(superblock, block_bitmap, owners, inode.indirect_block)?;
    let mut pointers = vec![0_u8; BLOCK_SIZE];
    read_block(device, superblock, inode.indirect_block, &mut pointers)?;
    let indirect_count =
        usize::try_from(expected - DIRECT_BLOCKS as u64).map_err(|_| FsError::FileTooLarge)?;
    for index in 0..INDIRECT_POINTERS {
        let offset = index * 8;
        let pointer = u64::from_le_bytes(pointers[offset..offset + 8].try_into().unwrap());
        if index < indirect_count {
            claim_data_block(superblock, block_bitmap, owners, pointer)?;
        } else if pointer != 0 {
            return Err(FsError::CorruptInode);
        }
    }
    Ok(())
}

fn claim_data_block(
    superblock: &Superblock,
    block_bitmap: &[u8],
    owners: &mut [u8],
    block: u64,
) -> Result<(), FsError> {
    if block < superblock.data_start_block
        || block >= superblock.total_blocks
        || !bitmap_bit(block_bitmap, block)?
    {
        return Err(FsError::InvalidLayout);
    }
    let owner = owners
        .get_mut(usize::try_from(block).map_err(|_| FsError::TooLarge)?)
        .ok_or(FsError::InvalidLayout)?;
    if *owner != 0 {
        return Err(FsError::InvalidLayout);
    }
    *owner = 2;
    Ok(())
}

fn read_all_inode_data<D: BlockDevice>(
    device: &mut D,
    superblock: &Superblock,
    inode: &Inode,
) -> Result<Vec<u8>, FsError> {
    let length = usize::try_from(inode.size).map_err(|_| FsError::TooLarge)?;
    let mut output = vec![0_u8; length];
    if output.is_empty() {
        return Ok(output);
    }
    let mut indirect = None;
    if inode.block_count > DIRECT_BLOCKS as u64 {
        let mut block = vec![0_u8; BLOCK_SIZE];
        read_block(device, superblock, inode.indirect_block, &mut block)?;
        indirect = Some(block);
    }
    for (index, chunk) in output.chunks_mut(BLOCK_SIZE).enumerate() {
        let block_number = if index < DIRECT_BLOCKS {
            inode.direct_blocks[index]
        } else {
            let pointers = indirect.as_ref().ok_or(FsError::CorruptInode)?;
            let offset = (index - DIRECT_BLOCKS) * 8;
            u64::from_le_bytes(pointers[offset..offset + 8].try_into().unwrap())
        };
        let mut block = vec![0_u8; BLOCK_SIZE];
        read_block(device, superblock, block_number, &mut block)?;
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
    Ok(output)
}

fn read_inode<D: BlockDevice>(
    device: &mut D,
    superblock: &Superblock,
    inode_number: u64,
) -> Result<Inode, FsError> {
    let inode_count = superblock.inode_count()?;
    if inode_number >= inode_count {
        return Err(FsError::InvalidLayout);
    }
    let byte_offset = inode_number
        .checked_mul(INODE_SIZE as u64)
        .ok_or(FsError::InvalidLayout)?;
    let block_number = superblock.inode_table_block + byte_offset / BLOCK_SIZE_U64;
    let offset = usize::try_from(byte_offset % BLOCK_SIZE_U64).map_err(|_| FsError::TooLarge)?;
    let mut block = vec![0_u8; BLOCK_SIZE];
    read_block(device, superblock, block_number, &mut block)?;
    Inode::decode(&block[offset..offset + INODE_SIZE])
}

fn read_block<D: BlockDevice>(
    device: &mut D,
    superblock: &Superblock,
    block: u64,
    output: &mut [u8],
) -> Result<(), FsError> {
    if output.len() != BLOCK_SIZE || block >= superblock.total_blocks {
        return Err(FsError::InvalidLayout);
    }
    let sectors_per_block = sectors_per_block(device)?;
    device
        .read_sectors(block * sectors_per_block, output)
        .map_err(FsError::Storage)
}

fn sectors_per_block<D: BlockDevice>(device: &D) -> Result<u64, FsError> {
    let sector_size = device.sector_size();
    if sector_size == 0 || !BLOCK_SIZE_U32.is_multiple_of(sector_size) {
        return Err(FsError::UnsupportedBlockSize);
    }
    Ok(u64::from(BLOCK_SIZE_U32 / sector_size))
}

fn device_bytes<D: BlockDevice>(device: &D) -> Result<u64, FsError> {
    device
        .sector_count()
        .checked_mul(u64::from(device.sector_size()))
        .ok_or(FsError::TooSmall)
}

fn bitmap_bit(bitmap: &[u8], bit: u64) -> Result<bool, FsError> {
    let byte = usize::try_from(bit / 8).map_err(|_| FsError::InvalidLayout)?;
    let shift = u32::try_from(bit % 8).map_err(|_| FsError::InvalidLayout)?;
    let value = bitmap.get(byte).ok_or(FsError::InvalidLayout)?;
    Ok(value & (1_u8 << shift) != 0)
}

fn set_bitmap_bit(bitmap: &mut [u8], bit: u64, value: bool) -> Result<(), FsError> {
    let byte = usize::try_from(bit / 8).map_err(|_| FsError::InvalidLayout)?;
    let shift = u32::try_from(bit % 8).map_err(|_| FsError::InvalidLayout)?;
    let target = bitmap.get_mut(byte).ok_or(FsError::InvalidLayout)?;
    if value {
        *target |= 1_u8 << shift;
    } else {
        *target &= !(1_u8 << shift);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexos_storage::MemoryBlockDevice;

    #[test]
    fn formats_and_checks_an_empty_root() {
        let mut disk = MemoryBlockDevice::new(4096, 512).unwrap();
        let uuid = [0x42; 16];
        let formatted = format(&mut disk, uuid).unwrap();
        let report = check_detailed(&mut disk).unwrap();
        assert_eq!(formatted, report.superblock);
        assert_eq!(report.superblock.uuid, uuid);
        assert_eq!(report.allocated_inodes, 1);
        assert_eq!(report.directories, 1);
        assert_eq!(report.files, 0);
        assert!(report.superblock.is_clean());
    }

    #[test]
    fn creates_nested_directories_and_multiblock_file() {
        let mut disk = MemoryBlockDevice::new(32_768, 512).unwrap();
        format(&mut disk, [1; 16]).unwrap();
        {
            let mut filesystem = NexFs::mount(&mut disk).unwrap();
            filesystem.create_dir("/etc").unwrap();
            filesystem.create_dir("/etc/nexos").unwrap();
            filesystem.create_file("/etc/nexos/config").unwrap();
            let input = vec![0x5a; BLOCK_SIZE * (DIRECT_BLOCKS + 1) + 37];
            assert_eq!(
                filesystem.write_file("/etc/nexos/config", 0, &input),
                Ok(input.len())
            );
            let mut output = vec![0_u8; input.len()];
            assert_eq!(
                filesystem.read_file("/etc/nexos/config", 0, &mut output),
                Ok(input.len())
            );
            assert_eq!(output, input);
            let stat = filesystem.stat("/etc/nexos/config").unwrap();
            assert_eq!(stat.blocks, (DIRECT_BLOCKS + 2) as u64);
            filesystem.unmount().unwrap();
        }
        let report = check_detailed(&mut disk).unwrap();
        assert_eq!(report.directories, 3);
        assert_eq!(report.files, 1);
        assert_eq!(report.directory_entries, 3);
    }

    #[test]
    fn renames_truncates_and_removes_entries() {
        let mut disk = MemoryBlockDevice::new(8192, 512).unwrap();
        format(&mut disk, [2; 16]).unwrap();
        {
            let mut filesystem = NexFs::mount(&mut disk).unwrap();
            filesystem.create_dir("/home").unwrap();
            filesystem.create_dir("/archive").unwrap();
            filesystem.create_file("/home/note").unwrap();
            filesystem
                .write_file("/home/note", 0, b"hello NexOS")
                .unwrap();
            filesystem.rename("/home/note", "/archive/readme").unwrap();
            assert_eq!(filesystem.stat("/home/note"), Err(FsError::NotFound));
            filesystem.truncate("/archive/readme", 5).unwrap();
            let mut output = [0_u8; 16];
            assert_eq!(
                filesystem.read_file("/archive/readme", 0, &mut output),
                Ok(5)
            );
            assert_eq!(&output[..5], b"hello");
            filesystem.remove("/archive/readme").unwrap();
            filesystem.remove("/archive").unwrap();
            filesystem.unmount().unwrap();
        }
        let report = check_detailed(&mut disk).unwrap();
        assert_eq!(report.directories, 2);
        assert_eq!(report.files, 0);
    }

    #[test]
    fn refuses_nonempty_directory_removal() {
        let mut disk = MemoryBlockDevice::new(4096, 512).unwrap();
        format(&mut disk, [3; 16]).unwrap();
        let mut filesystem = NexFs::mount(&mut disk).unwrap();
        filesystem.create_dir("/dir").unwrap();
        filesystem.create_file("/dir/file").unwrap();
        assert_eq!(filesystem.remove("/dir"), Err(FsError::DirectoryNotEmpty));
        filesystem.unmount().unwrap();
    }

    #[test]
    fn supports_random_writes_and_zero_filled_holes() {
        let mut disk = MemoryBlockDevice::new(8192, 512).unwrap();
        format(&mut disk, [5; 16]).unwrap();
        {
            let mut filesystem = NexFs::mount(&mut disk).unwrap();
            filesystem.create_file("/sparse").unwrap();
            let offset = BLOCK_SIZE as u64 * 2 + 17;
            filesystem.write_file("/sparse", offset, b"tail").unwrap();
            let mut output = vec![0xff; usize::try_from(offset).unwrap() + 4];
            assert_eq!(
                filesystem.read_file("/sparse", 0, &mut output),
                Ok(output.len())
            );
            assert!(
                output[..usize::try_from(offset).unwrap()]
                    .iter()
                    .all(|byte| *byte == 0)
            );
            assert_eq!(&output[usize::try_from(offset).unwrap()..], b"tail");
            filesystem.unmount().unwrap();
        }
        check(&mut disk).unwrap();
    }

    #[test]
    fn stores_directories_larger_than_one_block() {
        let mut disk = MemoryBlockDevice::new(8192, 512).unwrap();
        format(&mut disk, [6; 16]).unwrap();
        {
            let mut filesystem = NexFs::mount(&mut disk).unwrap();
            filesystem.create_dir("/many").unwrap();
            for index in 0..20 {
                filesystem
                    .create_file(&alloc::format!("/many/file-{index:02}"))
                    .unwrap();
            }
            assert_eq!(filesystem.read_dir("/many").unwrap().len(), 20);
            filesystem.unmount().unwrap();
        }
        assert_eq!(check_detailed(&mut disk).unwrap().directory_entries, 21);
    }

    #[test]
    fn checker_rejects_duplicate_data_block_ownership() {
        let mut disk = MemoryBlockDevice::new(8192, 512).unwrap();
        let superblock = format(&mut disk, [7; 16]).unwrap();
        let (first_number, second_number);
        {
            let mut filesystem = NexFs::mount(&mut disk).unwrap();
            first_number = filesystem.create_file("/first").unwrap();
            second_number = filesystem.create_file("/second").unwrap();
            filesystem.write_file("/first", 0, b"first").unwrap();
            filesystem.write_file("/second", 0, b"second").unwrap();
            filesystem.unmount().unwrap();
        }
        let first = read_inode(&mut disk, &superblock, first_number).unwrap();
        let mut second = read_inode(&mut disk, &superblock, second_number).unwrap();
        second.direct_blocks[0] = first.direct_blocks[0];
        let byte_offset = second_number * INODE_SIZE as u64;
        let block_number = superblock.inode_table_block + byte_offset / BLOCK_SIZE_U64;
        let inode_offset = usize::try_from(byte_offset % BLOCK_SIZE_U64).unwrap();
        let lba = block_number * (BLOCK_SIZE_U64 / 512);
        let mut block = vec![0_u8; BLOCK_SIZE];
        disk.read_sectors(lba, &mut block).unwrap();
        second
            .encode(&mut block[inode_offset..inode_offset + INODE_SIZE])
            .unwrap();
        disk.write_sectors(lba, &block).unwrap();
        assert_eq!(check(&mut disk), Err(FsError::InvalidLayout));
    }

    #[test]
    fn detects_dirty_and_corrupt_inode_metadata() {
        let mut disk = MemoryBlockDevice::new(4096, 512).unwrap();
        let superblock = format(&mut disk, [4; 16]).unwrap();
        {
            let _filesystem = NexFs::mount(&mut disk).unwrap();
        }
        assert_eq!(check(&mut disk), Err(FsError::Dirty));

        format(&mut disk, [4; 16]).unwrap();
        let lba = superblock.inode_table_block * (BLOCK_SIZE as u64 / 512);
        let mut block = vec![0_u8; BLOCK_SIZE];
        disk.read_sectors(lba, &mut block).unwrap();
        block[INODE_SIZE + 16] ^= 1;
        disk.write_sectors(lba, &block).unwrap();
        assert_eq!(check(&mut disk), Err(FsError::ChecksumMismatch));
    }

    #[test]
    fn rejects_tiny_volume() {
        let mut disk = MemoryBlockDevice::new(16, 512).unwrap();
        assert_eq!(format(&mut disk, [0; 16]), Err(FsError::TooSmall));
    }
}
