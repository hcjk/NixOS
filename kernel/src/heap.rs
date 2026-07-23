use core::ptr::NonNull;

use crate::memory::FrameAllocator;

const PAGE_SIZE: u64 = 4096;
const HEAP_PAGES: u64 = 64;

pub struct KernelHeap {
    physical_start: u64,
    virtual_start: u64,
    size: usize,
    used: usize,
    allocations: u64,
}

impl KernelHeap {
    pub fn initialize(allocator: &mut FrameAllocator, hhdm_offset: u64) -> Option<Self> {
        let physical_start = allocator.allocate_contiguous(HEAP_PAGES)?;
        let virtual_start = hhdm_offset.checked_add(physical_start)?;
        let size = usize::try_from(HEAP_PAGES * PAGE_SIZE).ok()?;

        // SAFETY: The frame allocator grants exclusive ownership of these
        // contiguous frames, and Limine maps them through the HHDM.
        unsafe { core::ptr::write_bytes(virtual_start as *mut u8, 0, size) };

        Some(Self {
            physical_start,
            virtual_start,
            size,
            used: 0,
            allocations: 0,
        })
    }

    pub fn allocate(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        let current = self.virtual_start.checked_add(self.used as u64)?;
        let aligned = current.checked_add((align - 1) as u64)? & !((align - 1) as u64);
        let end = aligned.checked_add(size as u64)?;
        let heap_end = self.virtual_start.checked_add(self.size as u64)?;
        if end > heap_end {
            return None;
        }
        self.used = usize::try_from(end - self.virtual_start).ok()?;
        self.allocations += 1;
        NonNull::new(aligned as *mut u8)
    }

    #[must_use]
    pub const fn physical_start(&self) -> u64 {
        self.physical_start
    }

    #[must_use]
    pub const fn virtual_start(&self) -> u64 {
        self.virtual_start
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    #[must_use]
    pub const fn allocations(&self) -> u64 {
        self.allocations
    }
}
