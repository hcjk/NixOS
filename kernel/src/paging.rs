use core::arch::asm;
use core::fmt;

use crate::memory::FrameAllocator;

const PAGE_SIZE: u64 = 4096;
const PAGE_SIZE_BYTES: usize = 4096;
const ENTRY_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const CACHE_DISABLE: u64 = 1 << 4;
const HUGE_PAGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;

#[derive(Clone, Copy)]
pub struct Mapping {
    pub physical_address: u64,
    pub page_size: u64,
    pub flags: u64,
}

pub struct PagingInfo {
    hhdm_offset: u64,
    level_4_frame: u64,
}

impl PagingInfo {
    #[must_use]
    pub fn detect(hhdm_offset: u64) -> Self {
        let cr3: u64;
        // SAFETY: Reading CR3 is valid at CPL0.
        unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
        Self {
            hhdm_offset,
            level_4_frame: cr3 & ENTRY_ADDRESS_MASK,
        }
    }

    #[must_use]
    pub const fn hhdm_offset(&self) -> u64 {
        self.hhdm_offset
    }

    #[must_use]
    pub const fn level_4_frame(&self) -> u64 {
        self.level_4_frame
    }

    #[must_use]
    pub fn translate(&self, virtual_address: u64) -> Option<Mapping> {
        let indices = [
            (virtual_address >> 39) & 0x1ff,
            (virtual_address >> 30) & 0x1ff,
            (virtual_address >> 21) & 0x1ff,
            (virtual_address >> 12) & 0x1ff,
        ];
        let mut table = self.level_4_frame;

        for (level, index) in indices.into_iter().enumerate() {
            let entry = self.read_entry(table, index)?;
            if entry & PRESENT == 0 {
                return None;
            }
            let frame = entry & ENTRY_ADDRESS_MASK;
            if level == 1 && entry & HUGE_PAGE != 0 {
                let page_size = 1024 * 1024 * 1024;
                return Some(Mapping {
                    physical_address: frame + (virtual_address & (page_size - 1)),
                    page_size,
                    flags: entry,
                });
            }
            if level == 2 && entry & HUGE_PAGE != 0 {
                let page_size = 2 * 1024 * 1024;
                return Some(Mapping {
                    physical_address: frame + (virtual_address & (page_size - 1)),
                    page_size,
                    flags: entry,
                });
            }
            if level == 3 {
                return Some(Mapping {
                    physical_address: frame + (virtual_address & 0xfff),
                    page_size: 4096,
                    flags: entry,
                });
            }
            table = frame;
        }
        None
    }

    pub fn map_page(
        &mut self,
        virtual_address: u64,
        physical_address: u64,
        flags: u64,
        allocator: &mut FrameAllocator,
    ) -> Result<(), PageMapError> {
        if virtual_address & (PAGE_SIZE - 1) != 0 || physical_address & (PAGE_SIZE - 1) != 0 {
            return Err(PageMapError::Unaligned);
        }
        let indices = [
            (virtual_address >> 39) & 0x1ff,
            (virtual_address >> 30) & 0x1ff,
            (virtual_address >> 21) & 0x1ff,
            (virtual_address >> 12) & 0x1ff,
        ];
        let mut table = self.level_4_frame;
        for index in indices[..3].iter().copied() {
            let entry_pointer = self.entry_pointer(table, index)?;
            // SAFETY: The table and index identify a page-table entry mapped
            // through the HHDM.
            let mut entry = unsafe { core::ptr::read_volatile(entry_pointer) };
            if entry & PRESENT == 0 {
                let new_table = allocator.allocate().ok_or(PageMapError::OutOfFrames)?;
                let virtual_table = self
                    .hhdm_offset
                    .checked_add(new_table)
                    .ok_or(PageMapError::AddressOverflow)?;
                // SAFETY: The allocator granted exclusive ownership of one
                // physical frame and the HHDM maps it.
                unsafe {
                    core::ptr::write_bytes(virtual_table as *mut u8, 0, PAGE_SIZE_BYTES);
                }
                entry = new_table | PRESENT | WRITABLE;
                // SAFETY: This is the inactive child entry being created.
                unsafe { core::ptr::write_volatile(entry_pointer, entry) };
            } else if entry & HUGE_PAGE != 0 {
                return Err(PageMapError::HugePageConflict);
            }
            table = entry & ENTRY_ADDRESS_MASK;
        }
        let leaf = self.entry_pointer(table, indices[3])?;
        // SAFETY: The leaf pointer addresses the active P1 table.
        if unsafe { core::ptr::read_volatile(leaf) } & PRESENT != 0 {
            return Err(PageMapError::AlreadyMapped);
        }
        // SAFETY: Installing a page-aligned physical frame with caller-selected
        // leaf flags creates one valid PTE.
        unsafe {
            core::ptr::write_volatile(leaf, physical_address | flags | PRESENT);
            asm!(
                "invlpg [{}]",
                in(reg) virtual_address,
                options(nostack, preserves_flags)
            );
        }
        Ok(())
    }

    pub fn map_mmio_page(
        &mut self,
        virtual_address: u64,
        physical_address: u64,
        allocator: &mut FrameAllocator,
    ) -> Result<(), PageMapError> {
        self.map_page(
            virtual_address,
            physical_address,
            WRITABLE | CACHE_DISABLE | NO_EXECUTE,
            allocator,
        )
    }

    pub fn map_writable_page(
        &mut self,
        virtual_address: u64,
        physical_address: u64,
        allocator: &mut FrameAllocator,
    ) -> Result<(), PageMapError> {
        self.map_page(
            virtual_address,
            physical_address,
            WRITABLE | NO_EXECUTE,
            allocator,
        )
    }

    pub fn unmap_page(&mut self, virtual_address: u64) -> Result<u64, PageMapError> {
        if virtual_address & (PAGE_SIZE - 1) != 0 {
            return Err(PageMapError::Unaligned);
        }
        let indices = [
            (virtual_address >> 39) & 0x1ff,
            (virtual_address >> 30) & 0x1ff,
            (virtual_address >> 21) & 0x1ff,
            (virtual_address >> 12) & 0x1ff,
        ];
        let mut table = self.level_4_frame;
        for index in indices[..3].iter().copied() {
            let entry = self
                .read_entry(table, index)
                .ok_or(PageMapError::NotMapped)?;
            if entry & PRESENT == 0 {
                return Err(PageMapError::NotMapped);
            }
            if entry & HUGE_PAGE != 0 {
                return Err(PageMapError::HugePageConflict);
            }
            table = entry & ENTRY_ADDRESS_MASK;
        }
        let leaf = self.entry_pointer(table, indices[3])?;
        // SAFETY: The pointer addresses the active P1 table.
        let entry = unsafe { core::ptr::read_volatile(leaf) };
        if entry & PRESENT == 0 {
            return Err(PageMapError::NotMapped);
        }
        // SAFETY: Clearing the PTE removes precisely the requested 4 KiB
        // mapping; invalidation prevents stale translations.
        unsafe {
            core::ptr::write_volatile(leaf, 0);
            asm!(
                "invlpg [{}]",
                in(reg) virtual_address,
                options(nostack, preserves_flags)
            );
        }
        Ok(entry & ENTRY_ADDRESS_MASK)
    }

    fn read_entry(&self, table_frame: u64, index: u64) -> Option<u64> {
        let address = self.entry_pointer(table_frame, index).ok()?;
        // SAFETY: Limine's HHDM maps physical memory, and each table frame came
        // from a present page-table entry or CR3. The index is restricted to
        // nine bits.
        Some(unsafe { core::ptr::read_volatile(address) })
    }

    fn entry_pointer(&self, table_frame: u64, index: u64) -> Result<*mut u64, PageMapError> {
        if index >= 512 {
            return Err(PageMapError::InvalidIndex);
        }
        let address = self
            .hhdm_offset
            .checked_add(table_frame)
            .and_then(|base| base.checked_add(index * 8))
            .ok_or(PageMapError::AddressOverflow)?;
        Ok(address as *mut u64)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PageMapError {
    Unaligned,
    InvalidIndex,
    AddressOverflow,
    OutOfFrames,
    HugePageConflict,
    AlreadyMapped,
    NotMapped,
}

impl fmt::Display for PageMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
