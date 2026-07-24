use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use nexos_storage::BlockDevice;

use crate::layout::{DirectoryEntry, FileType, Inode, data_blocks_for_size, validate_name};
use crate::{
    BLOCK_SIZE, DIR_ENTRY_SIZE, DIRECT_BLOCKS, FsError, INDIRECT_POINTERS, INODE_SIZE,
    MAX_FILE_BLOCKS, Superblock, check_detailed, write_superblock,
};

const DEFAULT_FILE_PERMISSIONS: u16 = 0o644;
const DEFAULT_DIRECTORY_PERMISSIONS: u16 = 0o755;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStat {
    pub inode: u64,
    pub kind: FileType,
    pub permissions: u16,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub blocks: u64,
}

pub struct NexFs<'a, D: BlockDevice> {
    device: &'a mut D,
    superblock: Superblock,
}

impl<'a, D: BlockDevice> NexFs<'a, D> {
    pub fn mount(device: &'a mut D) -> Result<Self, FsError> {
        let report = check_detailed(device)?;
        let mut superblock = report.superblock;
        superblock.flags &= !crate::CLEAN_FLAG;
        superblock.generation = superblock.generation.saturating_add(1);
        write_superblock(device, &superblock)?;
        device.flush()?;
        Ok(Self { device, superblock })
    }

    #[must_use]
    pub const fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub fn unmount(mut self) -> Result<Superblock, FsError> {
        self.device.flush()?;
        self.superblock.flags |= crate::CLEAN_FLAG;
        self.superblock.generation = self.superblock.generation.saturating_add(1);
        write_superblock(self.device, &self.superblock)?;
        self.device.flush()?;
        Ok(self.superblock)
    }

    pub fn stat(&mut self, path: &str) -> Result<FileStat, FsError> {
        let inode_number = self.resolve(path)?;
        let inode = self.read_inode(inode_number)?;
        Ok(FileStat {
            inode: inode_number,
            kind: inode.kind,
            permissions: inode.permissions,
            size: inode.size,
            created: inode.created,
            modified: inode.modified,
            blocks: inode.block_count,
        })
    }

    pub fn read_dir(&mut self, path: &str) -> Result<Vec<DirectoryEntry>, FsError> {
        let inode_number = self.resolve(path)?;
        let inode = self.read_inode(inode_number)?;
        self.read_directory(&inode)
    }

    pub fn create_file(&mut self, path: &str) -> Result<u64, FsError> {
        self.create(path, FileType::Regular, DEFAULT_FILE_PERMISSIONS)
    }

    pub fn create_dir(&mut self, path: &str) -> Result<u64, FsError> {
        self.create(path, FileType::Directory, DEFAULT_DIRECTORY_PERMISSIONS)
    }

    pub fn read_file(
        &mut self,
        path: &str,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        let inode_number = self.resolve(path)?;
        let inode = self.read_inode(inode_number)?;
        if inode.kind != FileType::Regular {
            return Err(FsError::IsDirectory);
        }
        self.read_inode_data(&inode, offset, output)
    }

    pub fn write_file(&mut self, path: &str, offset: u64, input: &[u8]) -> Result<usize, FsError> {
        let inode_number = self.resolve(path)?;
        let mut inode = self.read_inode(inode_number)?;
        if inode.kind != FileType::Regular {
            return Err(FsError::IsDirectory);
        }
        let end = offset
            .checked_add(input.len() as u64)
            .ok_or(FsError::FileTooLarge)?;
        Self::ensure_file_size_supported(end)?;
        if offset > inode.size {
            let old_size = inode.size;
            self.zero_range(&mut inode, old_size, offset - old_size)?;
        }
        self.write_inode_data(&mut inode, offset, input)?;
        inode.size = inode.size.max(end);
        let timestamp = self.next_timestamp();
        inode.modified = timestamp;
        inode.generation = timestamp;
        self.write_inode(inode_number, &inode)?;
        self.finish_mutation()?;
        Ok(input.len())
    }

    pub fn truncate(&mut self, path: &str, new_size: u64) -> Result<(), FsError> {
        let inode_number = self.resolve(path)?;
        let mut inode = self.read_inode(inode_number)?;
        if inode.kind != FileType::Regular {
            return Err(FsError::IsDirectory);
        }
        Self::ensure_file_size_supported(new_size)?;
        if new_size > inode.size {
            let old_size = inode.size;
            self.zero_range(&mut inode, old_size, new_size - old_size)?;
        } else if new_size < inode.size {
            self.truncate_inode_blocks(&mut inode, data_blocks_for_size(new_size))?;
            if new_size != 0 && !new_size.is_multiple_of(BLOCK_SIZE as u64) {
                let block_index = new_size / BLOCK_SIZE as u64;
                let block_number = self
                    .data_block(&inode, block_index)?
                    .ok_or(FsError::CorruptInode)?;
                let mut block = vec![0_u8; BLOCK_SIZE];
                self.read_block(block_number, &mut block)?;
                let start = usize::try_from(new_size % BLOCK_SIZE as u64)
                    .map_err(|_| FsError::FileTooLarge)?;
                block[start..].fill(0);
                self.write_block(block_number, &block)?;
            }
        }
        inode.size = new_size;
        let timestamp = self.next_timestamp();
        inode.modified = timestamp;
        inode.generation = timestamp;
        self.write_inode(inode_number, &inode)?;
        self.finish_mutation()
    }

    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        let (parent_path, name) = split_parent(path)?;
        let parent_number = self.resolve(parent_path)?;
        if parent_number == self.superblock.root_inode && name.is_empty() {
            return Err(FsError::Busy);
        }
        let mut parent = self.read_inode(parent_number)?;
        let mut entries = self.read_directory(&parent)?;
        let position = entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or(FsError::NotFound)?;
        let target_number = entries[position].inode;
        if target_number == self.superblock.root_inode {
            return Err(FsError::Busy);
        }
        let mut target = self.read_inode(target_number)?;
        if target.kind == FileType::Directory && !self.read_directory(&target)?.is_empty() {
            return Err(FsError::DirectoryNotEmpty);
        }

        entries.swap_remove(position);
        self.replace_directory(&mut parent, &entries)?;
        let timestamp = self.next_timestamp();
        parent.modified = timestamp;
        parent.generation = timestamp;
        self.write_inode(parent_number, &parent)?;

        self.truncate_inode_blocks(&mut target, 0)?;
        self.clear_inode(target_number)?;
        self.set_inode_allocated(target_number, false)?;
        self.finish_mutation()
    }

    pub fn rename(&mut self, source: &str, destination: &str) -> Result<(), FsError> {
        let (source_parent_path, source_name) = split_parent(source)?;
        let (destination_parent_path, destination_name) = split_parent(destination)?;
        validate_name(destination_name)?;
        let source_parent_number = self.resolve(source_parent_path)?;
        let destination_parent_number = self.resolve(destination_parent_path)?;

        let mut source_parent = self.read_inode(source_parent_number)?;
        let mut source_entries = self.read_directory(&source_parent)?;
        let source_position = source_entries
            .iter()
            .position(|entry| entry.name == source_name)
            .ok_or(FsError::NotFound)?;
        let source_entry = source_entries[source_position].clone();
        if source_entry.inode == self.superblock.root_inode {
            return Err(FsError::Busy);
        }

        let mut destination_parent = if destination_parent_number == source_parent_number {
            source_parent
        } else {
            self.read_inode(destination_parent_number)?
        };
        let mut destination_entries = if destination_parent_number == source_parent_number {
            source_entries.clone()
        } else {
            self.read_directory(&destination_parent)?
        };
        if destination_entries
            .iter()
            .any(|entry| entry.name == destination_name)
        {
            return Err(FsError::AlreadyExists);
        }
        if source_entry.kind == FileType::Directory
            && self.directory_contains(source_entry.inode, destination_parent_number)?
        {
            return Err(FsError::InvalidPath);
        }

        if source_parent_number == destination_parent_number {
            source_entries[source_position].name = String::from(destination_name);
            self.replace_directory(&mut source_parent, &source_entries)?;
            let timestamp = self.next_timestamp();
            source_parent.modified = timestamp;
            source_parent.generation = timestamp;
            self.write_inode(source_parent_number, &source_parent)?;
        } else {
            destination_entries.push(DirectoryEntry::new(
                source_entry.inode,
                source_entry.kind,
                destination_name,
            )?);
            self.replace_directory(&mut destination_parent, &destination_entries)?;
            let timestamp = self.next_timestamp();
            destination_parent.modified = timestamp;
            destination_parent.generation = timestamp;
            self.write_inode(destination_parent_number, &destination_parent)?;

            source_entries.swap_remove(source_position);
            self.replace_directory(&mut source_parent, &source_entries)?;
            source_parent.modified = timestamp;
            source_parent.generation = timestamp;
            self.write_inode(source_parent_number, &source_parent)?;
        }
        self.finish_mutation()
    }

    fn create(&mut self, path: &str, kind: FileType, permissions: u16) -> Result<u64, FsError> {
        let (parent_path, name) = split_parent(path)?;
        validate_name(name)?;
        let parent_number = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_number)?;
        if parent.kind != FileType::Directory {
            return Err(FsError::NotDirectory);
        }
        let mut entries = self.read_directory(&parent)?;
        if entries.iter().any(|entry| entry.name == name) {
            return Err(FsError::AlreadyExists);
        }
        let inode_number = self.allocate_inode()?;
        let timestamp = self.next_timestamp();
        let inode = Inode::new(kind, permissions, timestamp);
        self.write_inode(inode_number, &inode)?;
        entries.push(DirectoryEntry::new(inode_number, kind, name)?);
        self.replace_directory(&mut parent, &entries)?;
        parent.modified = timestamp;
        parent.generation = timestamp;
        self.write_inode(parent_number, &parent)?;
        self.finish_mutation()?;
        Ok(inode_number)
    }

    fn resolve(&mut self, path: &str) -> Result<u64, FsError> {
        let mut current = self.superblock.root_inode;
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(current);
        }
        for component in trimmed.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".." {
                return Err(FsError::InvalidPath);
            }
            validate_name(component)?;
            let inode = self.read_inode(current)?;
            let entries = self.read_directory(&inode)?;
            current = entries
                .iter()
                .find(|entry| entry.name == component)
                .map(|entry| entry.inode)
                .ok_or(FsError::NotFound)?;
        }
        Ok(current)
    }

    fn directory_contains(&mut self, directory: u64, target: u64) -> Result<bool, FsError> {
        if directory == target {
            return Ok(true);
        }
        let mut pending = vec![directory];
        let inode_count = self.superblock.inode_count()?;
        let mut visited =
            vec![false; usize::try_from(inode_count).map_err(|_| FsError::InvalidLayout)?];
        while let Some(current) = pending.pop() {
            let index = usize::try_from(current).map_err(|_| FsError::InvalidLayout)?;
            if index >= visited.len() || visited[index] {
                continue;
            }
            visited[index] = true;
            let inode = self.read_inode(current)?;
            for entry in self.read_directory(&inode)? {
                if entry.inode == target {
                    return Ok(true);
                }
                if entry.kind == FileType::Directory {
                    pending.push(entry.inode);
                }
            }
        }
        Ok(false)
    }

    fn read_directory(&mut self, inode: &Inode) -> Result<Vec<DirectoryEntry>, FsError> {
        if inode.kind != FileType::Directory {
            return Err(FsError::NotDirectory);
        }
        if !inode.size.is_multiple_of(DIR_ENTRY_SIZE as u64) {
            return Err(FsError::CorruptDirectory);
        }
        let entry_count =
            usize::try_from(inode.size / DIR_ENTRY_SIZE as u64).map_err(|_| FsError::TooLarge)?;
        let mut entries = Vec::with_capacity(entry_count);
        let mut encoded = [0_u8; DIR_ENTRY_SIZE];
        for index in 0..entry_count {
            let offset = (index * DIR_ENTRY_SIZE) as u64;
            if self.read_inode_data(inode, offset, &mut encoded)? != DIR_ENTRY_SIZE {
                return Err(FsError::CorruptDirectory);
            }
            entries.push(DirectoryEntry::decode(&encoded)?);
        }
        Ok(entries)
    }

    fn replace_directory(
        &mut self,
        inode: &mut Inode,
        entries: &[DirectoryEntry],
    ) -> Result<(), FsError> {
        if inode.kind != FileType::Directory {
            return Err(FsError::NotDirectory);
        }
        let new_size = entries
            .len()
            .checked_mul(DIR_ENTRY_SIZE)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(FsError::TooLarge)?;
        Self::ensure_file_size_supported(new_size)?;
        self.truncate_inode_blocks(inode, 0)?;
        inode.size = 0;
        let mut encoded = [0_u8; DIR_ENTRY_SIZE];
        for entry in entries {
            entry.encode(&mut encoded)?;
            self.write_inode_data(inode, inode.size, &encoded)?;
            inode.size += DIR_ENTRY_SIZE as u64;
        }
        Ok(())
    }

    fn read_inode_data(
        &mut self,
        inode: &Inode,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        if offset >= inode.size || output.is_empty() {
            return Ok(0);
        }
        let available = inode.size - offset;
        let length = output
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let mut completed = 0;
        let mut block = vec![0_u8; BLOCK_SIZE];
        while completed < length {
            let absolute = offset + completed as u64;
            let block_index = absolute / BLOCK_SIZE as u64;
            let within =
                usize::try_from(absolute % BLOCK_SIZE as u64).map_err(|_| FsError::FileTooLarge)?;
            let block_number = self
                .data_block(inode, block_index)?
                .ok_or(FsError::CorruptInode)?;
            self.read_block(block_number, &mut block)?;
            let amount = (BLOCK_SIZE - within).min(length - completed);
            output[completed..completed + amount].copy_from_slice(&block[within..within + amount]);
            completed += amount;
        }
        Ok(completed)
    }

    fn write_inode_data(
        &mut self,
        inode: &mut Inode,
        offset: u64,
        input: &[u8],
    ) -> Result<(), FsError> {
        if input.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(input.len() as u64)
            .ok_or(FsError::FileTooLarge)?;
        Self::ensure_file_size_supported(end)?;
        self.ensure_data_blocks(inode, data_blocks_for_size(end))?;
        let mut completed = 0;
        let mut block = vec![0_u8; BLOCK_SIZE];
        while completed < input.len() {
            let absolute = offset + completed as u64;
            let block_index = absolute / BLOCK_SIZE as u64;
            let within =
                usize::try_from(absolute % BLOCK_SIZE as u64).map_err(|_| FsError::FileTooLarge)?;
            let block_number = self
                .data_block(inode, block_index)?
                .ok_or(FsError::CorruptInode)?;
            let amount = (BLOCK_SIZE - within).min(input.len() - completed);
            if within != 0 || amount != BLOCK_SIZE {
                self.read_block(block_number, &mut block)?;
            } else {
                block.fill(0);
            }
            block[within..within + amount].copy_from_slice(&input[completed..completed + amount]);
            self.write_block(block_number, &block)?;
            completed += amount;
        }
        Ok(())
    }

    fn zero_range(&mut self, inode: &mut Inode, offset: u64, length: u64) -> Result<(), FsError> {
        if length == 0 {
            return Ok(());
        }
        let end = offset.checked_add(length).ok_or(FsError::FileTooLarge)?;
        Self::ensure_file_size_supported(end)?;
        self.ensure_data_blocks(inode, data_blocks_for_size(end))?;
        let zeros = vec![0_u8; BLOCK_SIZE];
        let mut completed = 0_u64;
        while completed < length {
            let amount = usize::try_from((length - completed).min(BLOCK_SIZE as u64))
                .map_err(|_| FsError::FileTooLarge)?;
            self.write_inode_data(inode, offset + completed, &zeros[..amount])?;
            completed += amount as u64;
        }
        Ok(())
    }

    fn ensure_file_size_supported(size: u64) -> Result<(), FsError> {
        if data_blocks_for_size(size) > MAX_FILE_BLOCKS as u64 {
            return Err(FsError::FileTooLarge);
        }
        Ok(())
    }

    fn ensure_data_blocks(&mut self, inode: &mut Inode, count: u64) -> Result<(), FsError> {
        if count > MAX_FILE_BLOCKS as u64 {
            return Err(FsError::FileTooLarge);
        }
        while inode.block_count < count {
            let index = inode.block_count;
            let block = self.allocate_block()?;
            if index < DIRECT_BLOCKS as u64 {
                let direct_index = usize::try_from(index).map_err(|_| FsError::FileTooLarge)?;
                inode.direct_blocks[direct_index] = block;
            } else {
                if inode.indirect_block == 0 {
                    inode.indirect_block = self.allocate_block()?;
                }
                let mut pointers = vec![0_u8; BLOCK_SIZE];
                self.read_block(inode.indirect_block, &mut pointers)?;
                let pointer_index =
                    usize::try_from(index - DIRECT_BLOCKS as u64).map_err(|_| FsError::TooLarge)?;
                if pointer_index >= INDIRECT_POINTERS {
                    return Err(FsError::FileTooLarge);
                }
                let offset = pointer_index * 8;
                pointers[offset..offset + 8].copy_from_slice(&block.to_le_bytes());
                self.write_block(inode.indirect_block, &pointers)?;
            }
            inode.block_count += 1;
        }
        Ok(())
    }

    fn truncate_inode_blocks(&mut self, inode: &mut Inode, keep: u64) -> Result<(), FsError> {
        if keep > inode.block_count {
            return self.ensure_data_blocks(inode, keep);
        }
        while inode.block_count > keep {
            let index = inode.block_count - 1;
            let block = self
                .data_block(inode, index)?
                .ok_or(FsError::CorruptInode)?;
            self.set_block_allocated(block, false)?;
            if index < DIRECT_BLOCKS as u64 {
                let direct_index = usize::try_from(index).map_err(|_| FsError::FileTooLarge)?;
                inode.direct_blocks[direct_index] = 0;
            } else {
                let mut pointers = vec![0_u8; BLOCK_SIZE];
                self.read_block(inode.indirect_block, &mut pointers)?;
                let pointer_index =
                    usize::try_from(index - DIRECT_BLOCKS as u64).map_err(|_| FsError::TooLarge)?;
                let offset = pointer_index * 8;
                pointers[offset..offset + 8].fill(0);
                self.write_block(inode.indirect_block, &pointers)?;
            }
            inode.block_count -= 1;
        }
        if inode.block_count <= DIRECT_BLOCKS as u64 && inode.indirect_block != 0 {
            self.set_block_allocated(inode.indirect_block, false)?;
            inode.indirect_block = 0;
        }
        Ok(())
    }

    fn data_block(&mut self, inode: &Inode, index: u64) -> Result<Option<u64>, FsError> {
        if index >= inode.block_count {
            return Ok(None);
        }
        if index < DIRECT_BLOCKS as u64 {
            let direct_index = usize::try_from(index).map_err(|_| FsError::FileTooLarge)?;
            let block = inode.direct_blocks[direct_index];
            return if block == 0 {
                Err(FsError::CorruptInode)
            } else {
                Ok(Some(block))
            };
        }
        if inode.indirect_block == 0 {
            return Err(FsError::CorruptInode);
        }
        let pointer_index =
            usize::try_from(index - DIRECT_BLOCKS as u64).map_err(|_| FsError::FileTooLarge)?;
        if pointer_index >= INDIRECT_POINTERS {
            return Err(FsError::FileTooLarge);
        }
        let mut pointers = vec![0_u8; BLOCK_SIZE];
        self.read_block(inode.indirect_block, &mut pointers)?;
        let offset = pointer_index * 8;
        let block = u64::from_le_bytes(pointers[offset..offset + 8].try_into().unwrap());
        if block == 0 {
            Err(FsError::CorruptInode)
        } else {
            Ok(Some(block))
        }
    }

    fn read_inode(&mut self, inode_number: u64) -> Result<Inode, FsError> {
        if !self.inode_allocated(inode_number)? {
            return Err(FsError::NotFound);
        }
        let (block_number, offset) = self.inode_location(inode_number)?;
        let mut block = vec![0_u8; BLOCK_SIZE];
        self.read_block(block_number, &mut block)?;
        Inode::decode(&block[offset..offset + INODE_SIZE])
    }

    fn write_inode(&mut self, inode_number: u64, inode: &Inode) -> Result<(), FsError> {
        let (block_number, offset) = self.inode_location(inode_number)?;
        let mut block = vec![0_u8; BLOCK_SIZE];
        self.read_block(block_number, &mut block)?;
        inode.encode(&mut block[offset..offset + INODE_SIZE])?;
        self.write_block(block_number, &block)
    }

    fn clear_inode(&mut self, inode_number: u64) -> Result<(), FsError> {
        let (block_number, offset) = self.inode_location(inode_number)?;
        let mut block = vec![0_u8; BLOCK_SIZE];
        self.read_block(block_number, &mut block)?;
        block[offset..offset + INODE_SIZE].fill(0);
        self.write_block(block_number, &block)
    }

    fn inode_location(&self, inode_number: u64) -> Result<(u64, usize), FsError> {
        let inode_count = self.superblock.inode_count()?;
        if inode_number >= inode_count {
            return Err(FsError::InvalidLayout);
        }
        let byte_offset = inode_number
            .checked_mul(INODE_SIZE as u64)
            .ok_or(FsError::InvalidLayout)?;
        let block = self.superblock.inode_table_block + byte_offset / BLOCK_SIZE as u64;
        let offset =
            usize::try_from(byte_offset % BLOCK_SIZE as u64).map_err(|_| FsError::InvalidLayout)?;
        Ok((block, offset))
    }

    fn allocate_inode(&mut self) -> Result<u64, FsError> {
        let inode_count = self.superblock.inode_count()?;
        for inode in 2..inode_count {
            if !self.inode_allocated(inode)? {
                self.set_inode_allocated(inode, true)?;
                return Ok(inode);
            }
        }
        Err(FsError::NoSpace)
    }

    fn inode_allocated(&mut self, inode: u64) -> Result<bool, FsError> {
        let inode_count = self.superblock.inode_count()?;
        if inode >= inode_count {
            return Err(FsError::InvalidLayout);
        }
        let mut bitmap = vec![0_u8; BLOCK_SIZE];
        self.read_block(self.superblock.inode_bitmap_block, &mut bitmap)?;
        bit_is_set(&bitmap, inode)
    }

    fn set_inode_allocated(&mut self, inode: u64, allocated: bool) -> Result<(), FsError> {
        let inode_count = self.superblock.inode_count()?;
        if inode >= inode_count {
            return Err(FsError::InvalidLayout);
        }
        let mut bitmap = vec![0_u8; BLOCK_SIZE];
        self.read_block(self.superblock.inode_bitmap_block, &mut bitmap)?;
        set_bit(&mut bitmap, inode, allocated)?;
        self.write_block(self.superblock.inode_bitmap_block, &bitmap)
    }

    fn allocate_block(&mut self) -> Result<u64, FsError> {
        for block in self.superblock.data_start_block..self.superblock.total_blocks {
            if !self.block_allocated(block)? {
                self.set_block_allocated(block, true)?;
                let zero = vec![0_u8; BLOCK_SIZE];
                self.write_block(block, &zero)?;
                return Ok(block);
            }
        }
        Err(FsError::NoSpace)
    }

    fn block_allocated(&mut self, block: u64) -> Result<bool, FsError> {
        if block >= self.superblock.total_blocks {
            return Err(FsError::InvalidLayout);
        }
        let bits_per_block = (BLOCK_SIZE * 8) as u64;
        let bitmap_block = self.superblock.block_bitmap_block + block / bits_per_block;
        let bit = block % bits_per_block;
        let mut bitmap = vec![0_u8; BLOCK_SIZE];
        self.read_block(bitmap_block, &mut bitmap)?;
        bit_is_set(&bitmap, bit)
    }

    fn set_block_allocated(&mut self, block: u64, allocated: bool) -> Result<(), FsError> {
        if block < self.superblock.data_start_block || block >= self.superblock.total_blocks {
            return Err(FsError::InvalidLayout);
        }
        let bits_per_block = (BLOCK_SIZE * 8) as u64;
        let bitmap_block = self.superblock.block_bitmap_block + block / bits_per_block;
        let bit = block % bits_per_block;
        let mut bitmap = vec![0_u8; BLOCK_SIZE];
        self.read_block(bitmap_block, &mut bitmap)?;
        set_bit(&mut bitmap, bit, allocated)?;
        self.write_block(bitmap_block, &bitmap)
    }

    fn read_block(&mut self, block: u64, output: &mut [u8]) -> Result<(), FsError> {
        if output.len() != BLOCK_SIZE || block >= self.superblock.total_blocks {
            return Err(FsError::InvalidLayout);
        }
        let sectors_per_block = crate::sectors_per_block(self.device)?;
        self.device
            .read_sectors(block * sectors_per_block, output)
            .map_err(FsError::Storage)
    }

    fn write_block(&mut self, block: u64, input: &[u8]) -> Result<(), FsError> {
        if input.len() != BLOCK_SIZE || block >= self.superblock.total_blocks {
            return Err(FsError::InvalidLayout);
        }
        let sectors_per_block = crate::sectors_per_block(self.device)?;
        self.device
            .write_sectors(block * sectors_per_block, input)
            .map_err(FsError::Storage)
    }

    fn next_timestamp(&mut self) -> u64 {
        self.superblock.generation = self.superblock.generation.saturating_add(1);
        self.superblock.generation
    }

    fn finish_mutation(&mut self) -> Result<(), FsError> {
        write_superblock(self.device, &self.superblock)?;
        self.device.flush()?;
        Ok(())
    }
}

fn split_parent(path: &str) -> Result<(&str, &str), FsError> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return Err(FsError::Busy);
    }
    let (parent, name) = trimmed
        .rsplit_once('/')
        .map_or(("/", trimmed), |(parent, name)| {
            (if parent.is_empty() { "/" } else { parent }, name)
        });
    validate_name(name)?;
    Ok((parent, name))
}

fn bit_is_set(bitmap: &[u8], bit: u64) -> Result<bool, FsError> {
    let byte = usize::try_from(bit / 8).map_err(|_| FsError::InvalidLayout)?;
    let shift = u32::try_from(bit % 8).map_err(|_| FsError::InvalidLayout)?;
    let value = bitmap.get(byte).ok_or(FsError::InvalidLayout)?;
    Ok(value & (1_u8 << shift) != 0)
}

fn set_bit(bitmap: &mut [u8], bit: u64, value: bool) -> Result<(), FsError> {
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
