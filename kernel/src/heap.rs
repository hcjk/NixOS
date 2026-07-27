use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::memory::FrameAllocator;

const PAGE_SIZE: u64 = 4096;
const HEAP_PAGES: u64 = 4096;
const MAX_SEGMENTS: usize = 128;

#[derive(Clone, Copy)]
struct Segment {
    offset: usize,
    size: usize,
    free: bool,
}

impl Segment {
    const EMPTY: Self = Self {
        offset: 0,
        size: 0,
        free: false,
    };
}

pub struct KernelHeap {
    physical_start: u64,
    virtual_start: u64,
    size: usize,
    segments: [Segment; MAX_SEGMENTS],
    segment_count: usize,
    used: usize,
    peak_used: usize,
    active_allocations: usize,
    total_allocations: u64,
    deallocations: u64,
}

#[derive(Clone, Copy)]
pub struct HeapStats {
    pub physical_start: u64,
    pub virtual_start: u64,
    pub size: usize,
    pub used: usize,
    pub free_bytes: usize,
    pub largest_free_block: usize,
    pub active_allocations: usize,
    pub peak_used: usize,
    pub total_allocations: u64,
    pub deallocations: u64,
}

struct GlobalKernelHeap {
    locked: AtomicBool,
    heap: UnsafeCell<Option<KernelHeap>>,
}

// SAFETY: All access to `heap` is serialized by the atomic spin lock.
unsafe impl Sync for GlobalKernelHeap {}

impl GlobalKernelHeap {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            heap: UnsafeCell::new(None),
        }
    }

    fn with_heap<R>(&self, operation: impl FnOnce(&mut KernelHeap) -> R) -> Option<R> {
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        // SAFETY: The lock grants this call exclusive access to the cell.
        let result = unsafe { &mut *self.heap.get() }.as_mut().map(operation);
        self.locked.store(false, Ordering::Release);
        result
    }

    fn initialize(&self, heap: KernelHeap) -> bool {
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        // SAFETY: The lock grants exclusive access and initialization happens
        // once before any allocation-backed subsystem is constructed.
        let slot = unsafe { &mut *self.heap.get() };
        let initialized = slot.is_none();
        if initialized {
            *slot = Some(heap);
        }
        self.locked.store(false, Ordering::Release);
        initialized
    }
}

// SAFETY: `KernelHeap` validates layouts and the wrapper serializes mutation.
unsafe impl GlobalAlloc for GlobalKernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with_heap(|heap| heap.allocate(layout.size(), layout.align()))
            .flatten()
            .map_or_else(null_mut, NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
        if let Some(pointer) = NonNull::new(pointer) {
            let _ = self.with_heap(|heap| heap.deallocate(pointer));
        }
    }
}

#[global_allocator]
static GLOBAL_HEAP: GlobalKernelHeap = GlobalKernelHeap::new();

pub fn initialize_global(allocator: &mut FrameAllocator, hhdm_offset: u64) -> Option<HeapStats> {
    let heap = KernelHeap::initialize(allocator, hhdm_offset)?;
    if !GLOBAL_HEAP.initialize(heap) {
        return None;
    }
    stats()
}

#[must_use]
pub fn stats() -> Option<HeapStats> {
    GLOBAL_HEAP.with_heap(|heap| HeapStats {
        physical_start: heap.physical_start(),
        virtual_start: heap.virtual_start(),
        size: heap.size(),
        used: heap.used(),
        free_bytes: heap.free_bytes(),
        largest_free_block: heap.largest_free_block(),
        active_allocations: heap.active_allocations(),
        peak_used: heap.peak_used(),
        total_allocations: heap.total_allocations(),
        deallocations: heap.deallocations(),
    })
}

pub fn self_test() -> Option<(usize, u64, bool, bool)> {
    let layout = Layout::from_size_align(64, 16).ok()?;
    // SAFETY: The returned pointer is checked for null and remains live until
    // it is passed back with the same layout.
    let allocation = unsafe { alloc::alloc::alloc(layout) };
    let allocation = NonNull::new(allocation)?;
    let mut checksum = 0_u64;
    for index in 0_u8..64 {
        let value = index.wrapping_mul(3).wrapping_add(1);
        // SAFETY: The allocation is 64 bytes and the index is in range.
        unsafe { allocation.as_ptr().add(usize::from(index)).write(value) };
        checksum += u64::from(value);
    }
    let first_address = allocation.as_ptr() as usize;
    // SAFETY: The pointer and layout match the live allocation above.
    unsafe { alloc::alloc::dealloc(allocation.as_ptr(), layout) };
    // SAFETY: The layout is valid and the returned pointer is checked.
    let second = NonNull::new(unsafe { alloc::alloc::alloc(layout) });
    let reused = second.is_some_and(|pointer| pointer.as_ptr() as usize == first_address);
    if let Some(second) = second {
        // SAFETY: The pointer and layout match the second live allocation.
        unsafe { alloc::alloc::dealloc(second.as_ptr(), layout) };
    }
    Some((first_address, checksum, true, reused))
}

impl KernelHeap {
    pub fn initialize(allocator: &mut FrameAllocator, hhdm_offset: u64) -> Option<Self> {
        let physical_start = allocator.allocate_contiguous(HEAP_PAGES)?;
        let virtual_start = hhdm_offset.checked_add(physical_start)?;
        let size = usize::try_from(HEAP_PAGES * PAGE_SIZE).ok()?;

        // SAFETY: The frame allocator grants exclusive ownership of these
        // contiguous frames, and Limine maps them through the HHDM.
        unsafe { core::ptr::write_bytes(virtual_start as *mut u8, 0, size) };

        let mut segments = [Segment::EMPTY; MAX_SEGMENTS];
        segments[0] = Segment {
            offset: 0,
            size,
            free: true,
        };
        Some(Self {
            physical_start,
            virtual_start,
            size,
            segments,
            segment_count: 1,
            used: 0,
            peak_used: 0,
            active_allocations: 0,
            total_allocations: 0,
            deallocations: 0,
        })
    }

    pub fn allocate(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        for index in 0..self.segment_count {
            let segment = self.segments[index];
            if !segment.free {
                continue;
            }
            let segment_address = usize::try_from(self.virtual_start)
                .ok()?
                .checked_add(segment.offset)?;
            let aligned_address = segment_address.checked_add(align - 1)? & !(align - 1);
            let aligned_offset =
                aligned_address.checked_sub(usize::try_from(self.virtual_start).ok()?)?;
            let allocation_end = aligned_offset.checked_add(size)?;
            let segment_end = segment.offset.checked_add(segment.size)?;
            if allocation_end > segment_end {
                continue;
            }

            let prefix_size = aligned_offset - segment.offset;
            let suffix_size = segment_end - allocation_end;
            let mut replacement = [Segment::EMPTY; 3];
            let mut replacement_count = 0;
            if prefix_size != 0 {
                replacement[replacement_count] = Segment {
                    offset: segment.offset,
                    size: prefix_size,
                    free: true,
                };
                replacement_count += 1;
            }
            replacement[replacement_count] = Segment {
                offset: aligned_offset,
                size,
                free: false,
            };
            replacement_count += 1;
            if suffix_size != 0 {
                replacement[replacement_count] = Segment {
                    offset: allocation_end,
                    size: suffix_size,
                    free: true,
                };
                replacement_count += 1;
            }
            if !self.replace_segment(index, replacement, replacement_count) {
                return None;
            }

            self.used += size;
            self.peak_used = self.peak_used.max(self.used);
            self.active_allocations += 1;
            self.total_allocations += 1;
            return NonNull::new(aligned_address as *mut u8);
        }
        None
    }

    pub fn deallocate(&mut self, pointer: NonNull<u8>) -> bool {
        let address = pointer.as_ptr() as usize;
        let Ok(heap_start) = usize::try_from(self.virtual_start) else {
            return false;
        };
        let Some(offset) = address.checked_sub(heap_start) else {
            return false;
        };
        let Some(mut index) = self.segments[..self.segment_count]
            .iter()
            .position(|segment| !segment.free && segment.offset == offset)
        else {
            return false;
        };

        self.segments[index].free = true;
        self.used -= self.segments[index].size;
        self.active_allocations -= 1;
        self.deallocations += 1;

        if index > 0 && self.segments[index - 1].free {
            self.segments[index - 1].size += self.segments[index].size;
            self.remove_segment(index);
            index -= 1;
        }
        if index + 1 < self.segment_count && self.segments[index + 1].free {
            self.segments[index].size += self.segments[index + 1].size;
            self.remove_segment(index + 1);
        }
        true
    }

    #[must_use]
    pub fn free_bytes(&self) -> usize {
        self.segments[..self.segment_count]
            .iter()
            .filter(|segment| segment.free)
            .map(|segment| segment.size)
            .sum()
    }

    #[must_use]
    pub fn largest_free_block(&self) -> usize {
        self.segments[..self.segment_count]
            .iter()
            .filter(|segment| segment.free)
            .map(|segment| segment.size)
            .max()
            .unwrap_or(0)
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
    pub const fn peak_used(&self) -> usize {
        self.peak_used
    }

    #[must_use]
    pub const fn active_allocations(&self) -> usize {
        self.active_allocations
    }

    #[must_use]
    pub const fn total_allocations(&self) -> u64 {
        self.total_allocations
    }

    #[must_use]
    pub const fn deallocations(&self) -> u64 {
        self.deallocations
    }

    fn replace_segment(
        &mut self,
        index: usize,
        replacement: [Segment; 3],
        replacement_count: usize,
    ) -> bool {
        let additional = replacement_count - 1;
        if self.segment_count + additional > MAX_SEGMENTS {
            return false;
        }
        for position in (index + 1..self.segment_count).rev() {
            self.segments[position + additional] = self.segments[position];
        }
        self.segments[index..index + replacement_count]
            .copy_from_slice(&replacement[..replacement_count]);
        self.segment_count += additional;
        true
    }

    fn remove_segment(&mut self, index: usize) {
        for position in index..self.segment_count - 1 {
            self.segments[position] = self.segments[position + 1];
        }
        self.segment_count -= 1;
        self.segments[self.segment_count] = Segment::EMPTY;
    }
}
