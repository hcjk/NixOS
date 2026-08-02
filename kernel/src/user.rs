use core::fmt;

use nexos_runtime::elf::{ElfError, ElfImage, LoadError, LoadSegment, SegmentTarget};

use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};
use crate::rootfs::RootFileSystem;
use crate::storage::StorageManager;

const PAGE_SIZE: u64 = 4096;
const PAGE_BYTES: usize = 4096;
const USER_STACK_BOTTOM: u64 = 0x0000_0000_7ffe_0000;
const USER_STACK_PAGES: usize = 16;

#[derive(Clone, Copy)]
pub struct LoadedProgram {
    pub entry: u64,
    pub stack_pointer: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum UserLoadError {
    Filesystem(nexos_abi::Error),
    InvalidElf(ElfError),
    Mapping(PageMapError),
    OutOfMemory,
    AddressOverflow,
}

impl fmt::Display for UserLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Filesystem(error) => write!(formatter, "filesystem error: {error:?}"),
            Self::InvalidElf(error) => write!(formatter, "invalid ELF: {error}"),
            Self::Mapping(error) => write!(formatter, "mapping error: {error}"),
            Self::OutOfMemory => formatter.write_str("out of physical memory"),
            Self::AddressOverflow => formatter.write_str("address overflow"),
        }
    }
}

pub fn load_program(
    root: &RootFileSystem,
    storage: &mut StorageManager,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
    path: &[u8],
) -> Result<LoadedProgram, UserLoadError> {
    let executable = root
        .read_all(storage, path)
        .map_err(UserLoadError::Filesystem)?;
    let image = ElfImage::parse(&executable).map_err(UserLoadError::InvalidElf)?;
    let hhdm_offset = paging.hhdm_offset();
    let mut target = AddressSpaceTarget {
        paging,
        allocator,
        hhdm_offset,
    };
    let entry = image.load_into(&mut target).map_err(|error| match error {
        LoadError::InvalidImage(error) => UserLoadError::InvalidElf(error),
        LoadError::Target(error) => error,
    })?;
    for page in 0..USER_STACK_PAGES {
        let address = USER_STACK_BOTTOM + (page as u64) * PAGE_SIZE;
        target.ensure_page(address, true, false)?;
        target.zero_virtual(address, PAGE_BYTES)?;
    }
    Ok(LoadedProgram {
        entry,
        // The ELF entry follows the SysV x86-64 convention and therefore
        // starts with RSP congruent to 8 modulo 16, as if reached by `call`.
        stack_pointer: USER_STACK_BOTTOM + USER_STACK_PAGES as u64 * PAGE_SIZE - 8,
    })
}

struct AddressSpaceTarget<'a> {
    paging: &'a mut PagingInfo,
    allocator: &'a mut FrameAllocator,
    hhdm_offset: u64,
}

impl AddressSpaceTarget<'_> {
    fn ensure_page(
        &mut self,
        virtual_address: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), UserLoadError> {
        if self.paging.translate(virtual_address).is_some() {
            return Ok(());
        }
        let frame = self
            .allocator
            .allocate()
            .ok_or(UserLoadError::OutOfMemory)?;
        self.paging
            .map_user_page(virtual_address, frame, writable, executable, self.allocator)
            .map_err(UserLoadError::Mapping)
    }

    fn zero_virtual(&self, virtual_address: u64, length: usize) -> Result<(), UserLoadError> {
        let mut completed = 0;
        while completed < length {
            let address = virtual_address
                .checked_add(completed as u64)
                .ok_or(UserLoadError::AddressOverflow)?;
            let mapping = self
                .paging
                .translate(address)
                .ok_or(UserLoadError::Mapping(PageMapError::NotMapped))?;
            let page_remaining = PAGE_BYTES - (address as usize & (PAGE_BYTES - 1));
            let amount = page_remaining.min(length - completed);
            let target = self
                .hhdm_offset
                .checked_add(mapping.physical_address)
                .ok_or(UserLoadError::AddressOverflow)?;
            // SAFETY: The loader owns each mapped user frame while preparing
            // the process, and the HHDM exposes the translated physical bytes.
            unsafe { core::ptr::write_bytes(target as *mut u8, 0, amount) };
            completed += amount;
        }
        Ok(())
    }

    fn copy_virtual(&self, virtual_address: u64, bytes: &[u8]) -> Result<(), UserLoadError> {
        let mut completed = 0;
        while completed < bytes.len() {
            let address = virtual_address
                .checked_add(completed as u64)
                .ok_or(UserLoadError::AddressOverflow)?;
            let mapping = self
                .paging
                .translate(address)
                .ok_or(UserLoadError::Mapping(PageMapError::NotMapped))?;
            let page_remaining = PAGE_BYTES - (address as usize & (PAGE_BYTES - 1));
            let amount = page_remaining.min(bytes.len() - completed);
            let target = self
                .hhdm_offset
                .checked_add(mapping.physical_address)
                .ok_or(UserLoadError::AddressOverflow)?;
            // SAFETY: Source and destination are valid for `amount`, do not
            // overlap, and the loader exclusively initializes the user frame.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes[completed..].as_ptr(),
                    target as *mut u8,
                    amount,
                );
            }
            completed += amount;
        }
        Ok(())
    }
}

impl SegmentTarget for AddressSpaceTarget<'_> {
    type Error = UserLoadError;

    fn load_segment(&mut self, segment: LoadSegment<'_>) -> Result<(), Self::Error> {
        let page_start = segment.virtual_address & !(PAGE_SIZE - 1);
        let segment_end = segment
            .virtual_address
            .checked_add(segment.memory_size)
            .ok_or(UserLoadError::AddressOverflow)?;
        let page_end = segment_end
            .checked_add(PAGE_SIZE - 1)
            .ok_or(UserLoadError::AddressOverflow)?
            & !(PAGE_SIZE - 1);
        let mut page = page_start;
        while page < page_end {
            self.ensure_page(page, segment.writable, segment.executable)?;
            page = page
                .checked_add(PAGE_SIZE)
                .ok_or(UserLoadError::AddressOverflow)?;
        }
        let memory_size =
            usize::try_from(segment.memory_size).map_err(|_| UserLoadError::AddressOverflow)?;
        self.zero_virtual(segment.virtual_address, memory_size)?;
        self.copy_virtual(segment.virtual_address, segment.file_data)?;
        Ok(())
    }
}
