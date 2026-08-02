use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::str;

use nexfs::{FileType as NexFileType, FsError, NexFs};
use nexos_abi::{DirectoryEntry, Error, FileStat, FileType, OPEN_DIRECTORY, PATH_MAX};
use nexos_runtime::vfs::Path;
use nexos_storage::{NEXFS_TYPE_GUID, PartitionDevice, read_partition_table};

use crate::storage::{StorageDevice, StorageManager};

const MAX_OPEN_FILES: usize = 16;
const FIRST_FILE_DESCRIPTOR: u32 = 3;

#[derive(Clone, Copy)]
pub struct RootMountInfo {
    pub disk_index: usize,
    pub partition_index: u32,
    pub first_lba: u64,
    pub sector_count: u64,
    pub uuid: [u8; 16],
}

#[derive(Clone, Copy)]
struct OpenFile {
    path: [u8; PATH_MAX],
    path_length: u16,
    offset: u64,
    directory_index: usize,
    directory: bool,
}

impl OpenFile {
    fn new(path: &[u8], directory: bool) -> Result<Self, Error> {
        let mut stored = [0_u8; PATH_MAX];
        stored
            .get_mut(..path.len())
            .ok_or(Error::NameTooLong)?
            .copy_from_slice(path);
        Ok(Self {
            path: stored,
            path_length: u16::try_from(path.len()).map_err(|_| Error::NameTooLong)?,
            offset: 0,
            directory_index: 0,
            directory,
        })
    }

    fn path(&self) -> Result<&str, Error> {
        str::from_utf8(&self.path[..usize::from(self.path_length)])
            .map_err(|_| Error::InvalidArgument)
    }
}

pub struct RootFileSystem {
    mount: RootMountInfo,
    handles: Box<[Option<OpenFile>]>,
}

impl RootFileSystem {
    pub fn discover(storage: &mut StorageManager) -> Option<Self> {
        for disk_index in 0..storage.count() {
            let device = storage.device_mut(disk_index)?;
            let Ok(table) = read_partition_table(device) else {
                continue;
            };
            for partition in table.partitions {
                if partition.type_guid != Some(NEXFS_TYPE_GUID) {
                    continue;
                }
                let Ok(mut device) =
                    PartitionDevice::new(device, partition.first_lba, partition.sector_count)
                else {
                    continue;
                };
                let Ok(superblock) = nexfs::inspect_superblock(&mut device) else {
                    continue;
                };
                return Some(Self {
                    mount: RootMountInfo {
                        disk_index,
                        partition_index: partition.index,
                        first_lba: partition.first_lba,
                        sector_count: partition.sector_count,
                        uuid: superblock.uuid,
                    },
                    handles: vec![None; MAX_OPEN_FILES].into_boxed_slice(),
                });
            }
        }
        None
    }

    #[must_use]
    pub const fn mount_info(&self) -> RootMountInfo {
        self.mount
    }

    pub fn read_all(&self, storage: &mut StorageManager, path: &[u8]) -> Result<Vec<u8>, Error> {
        let path = normalize(path)?;
        self.with_filesystem(storage, |filesystem| {
            let stat = filesystem.stat(path)?;
            if stat.kind != NexFileType::Regular {
                return Err(FsError::IsDirectory);
            }
            let length = usize::try_from(stat.size).map_err(|_| FsError::FileTooLarge)?;
            let mut bytes = vec![0_u8; length];
            let read = filesystem.read_file(path, 0, &mut bytes)?;
            if read != bytes.len() {
                return Err(FsError::Storage(nexos_storage::StorageError::Device));
            }
            Ok(bytes)
        })
    }

    pub fn open(
        &mut self,
        storage: &mut StorageManager,
        path: &[u8],
        flags: u32,
    ) -> Result<u32, Error> {
        let normalized = Path::normalize(path, b"/")?;
        let path = normalized.as_bytes();
        let path_text = str::from_utf8(path).map_err(|_| Error::InvalidArgument)?;
        let stat = self.with_filesystem(storage, |filesystem| filesystem.stat(path_text))?;
        let directory = flags & OPEN_DIRECTORY != 0;
        if directory && stat.kind != NexFileType::Directory {
            return Err(Error::NotDirectory);
        }
        if !directory && stat.kind == NexFileType::Directory {
            return Err(Error::IsDirectory);
        }
        let index = self
            .handles
            .iter()
            .position(Option::is_none)
            .ok_or(Error::HandleLimit)?;
        self.handles[index] = Some(OpenFile::new(path, directory)?);
        Ok(FIRST_FILE_DESCRIPTOR + u32::try_from(index).map_err(|_| Error::HandleLimit)?)
    }

    pub fn close(&mut self, descriptor: u32) -> Result<(), Error> {
        let index = handle_index(descriptor)?;
        let handle = self.handles.get_mut(index).ok_or(Error::BadHandle)?;
        handle.take().ok_or(Error::BadHandle)?;
        Ok(())
    }

    pub fn read(
        &mut self,
        storage: &mut StorageManager,
        descriptor: u32,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        let index = handle_index(descriptor)?;
        let mut handle = self
            .handles
            .get(index)
            .copied()
            .flatten()
            .ok_or(Error::BadHandle)?;
        if handle.directory {
            return Err(Error::IsDirectory);
        }
        let path = handle.path()?;
        let read = self.with_filesystem(storage, |filesystem| {
            filesystem.read_file(path, handle.offset, output)
        })?;
        handle.offset = handle.offset.saturating_add(read as u64);
        self.handles[index] = Some(handle);
        Ok(read)
    }

    pub fn stat(&self, storage: &mut StorageManager, path: &[u8]) -> Result<FileStat, Error> {
        let path = normalize(path)?;
        let stat = self.with_filesystem(storage, |filesystem| filesystem.stat(path))?;
        Ok(FileStat {
            inode: stat.inode,
            size: stat.size,
            modified_seconds: stat.modified,
            file_type: abi_file_type(stat.kind) as u32,
            permissions: stat.permissions,
            reserved: 0,
        })
    }

    pub fn read_dir(
        &mut self,
        storage: &mut StorageManager,
        descriptor: u32,
    ) -> Result<Option<DirectoryEntry>, Error> {
        let index = handle_index(descriptor)?;
        let mut handle = self
            .handles
            .get(index)
            .copied()
            .flatten()
            .ok_or(Error::BadHandle)?;
        if !handle.directory {
            return Err(Error::NotDirectory);
        }
        let path = handle.path()?;
        let entries = self.with_filesystem(storage, |filesystem| filesystem.read_dir(path))?;
        let Some(entry) = entries.get(handle.directory_index) else {
            return Ok(None);
        };
        let mut output = DirectoryEntry {
            inode: entry.inode,
            file_type: abi_file_type(entry.kind) as u32,
            ..DirectoryEntry::default()
        };
        let name = entry.name.as_bytes();
        let length = name.len().min(output.name.len());
        output.name[..length].copy_from_slice(&name[..length]);
        output.name_length = u16::try_from(length).map_err(|_| Error::NameTooLong)?;
        handle.directory_index += 1;
        self.handles[index] = Some(handle);
        Ok(Some(output))
    }

    fn with_filesystem<R>(
        &self,
        storage: &mut StorageManager,
        operation: impl FnOnce(&mut NexFs<'_, PartitionDevice<'_, StorageDevice>>) -> Result<R, FsError>,
    ) -> Result<R, Error> {
        let device = storage
            .device_mut(self.mount.disk_index)
            .ok_or(Error::NotFound)?;
        let mut partition =
            PartitionDevice::new(device, self.mount.first_lba, self.mount.sector_count)
                .map_err(storage_error)?;
        let mut filesystem = NexFs::mount(&mut partition).map_err(filesystem_error)?;
        let result = operation(&mut filesystem);
        let unmount = filesystem.unmount();
        match (result, unmount) {
            (Ok(value), Ok(_)) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(filesystem_error(error)),
        }
    }
}

fn normalize(path: &[u8]) -> Result<&str, Error> {
    str::from_utf8(path).map_err(|_| Error::InvalidArgument)
}

fn handle_index(descriptor: u32) -> Result<usize, Error> {
    let index = descriptor
        .checked_sub(FIRST_FILE_DESCRIPTOR)
        .ok_or(Error::BadHandle)?;
    usize::try_from(index).map_err(|_| Error::BadHandle)
}

const fn abi_file_type(kind: NexFileType) -> FileType {
    match kind {
        NexFileType::Regular => FileType::Regular,
        NexFileType::Directory => FileType::Directory,
    }
}

fn filesystem_error(error: FsError) -> Error {
    match error {
        FsError::NotFound => Error::NotFound,
        FsError::AlreadyExists => Error::AlreadyExists,
        FsError::NotDirectory => Error::NotDirectory,
        FsError::IsDirectory => Error::IsDirectory,
        FsError::InvalidName | FsError::InvalidPath => Error::InvalidArgument,
        FsError::NoSpace => Error::NoSpace,
        FsError::FileTooLarge | FsError::TooLarge => Error::FileTooLarge,
        FsError::Busy => Error::WouldBlock,
        FsError::Storage(error) => storage_error(error),
        _ => Error::Io,
    }
}

fn storage_error(error: nexos_storage::StorageError) -> Error {
    match error {
        nexos_storage::StorageError::ReadOnly => Error::ReadOnly,
        nexos_storage::StorageError::NotFound => Error::NotFound,
        nexos_storage::StorageError::AlreadyExists => Error::AlreadyExists,
        nexos_storage::StorageError::NoSpace => Error::NoSpace,
        nexos_storage::StorageError::Busy => Error::WouldBlock,
        _ => Error::Io,
    }
}
