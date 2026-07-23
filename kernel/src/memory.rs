use limine::memmap::MEMMAP_USABLE;
use limine::request::MemmapResponse;

const PAGE_SIZE: u64 = 4096;
const LOW_MEMORY_END: u64 = 0x10_0000;
const MAX_RANGES: usize = 64;

#[derive(Clone, Copy)]
struct FrameRange {
    next: u64,
    end: u64,
}

impl FrameRange {
    const EMPTY: Self = Self { next: 0, end: 0 };
}

pub struct FrameAllocator {
    ranges: [FrameRange; MAX_RANGES],
    range_count: usize,
    current_range: usize,
    total_frames: u64,
    allocated_frames: u64,
}

impl FrameAllocator {
    #[must_use]
    pub fn from_memory_map(memory_map: &MemmapResponse) -> Self {
        let mut allocator = Self {
            ranges: [FrameRange::EMPTY; MAX_RANGES],
            range_count: 0,
            current_range: 0,
            total_frames: 0,
            allocated_frames: 0,
        };
        for entry in memory_map.entries() {
            if entry.type_ != MEMMAP_USABLE || allocator.range_count == MAX_RANGES {
                continue;
            }
            // Keep the first MiB reserved for firmware data, legacy devices,
            // and the null-page guard even if firmware labels part of it usable.
            let start = align_up(entry.base.max(LOW_MEMORY_END), PAGE_SIZE);
            let end = align_down(entry.base.saturating_add(entry.length), PAGE_SIZE);
            if start >= end {
                continue;
            }
            allocator.ranges[allocator.range_count] = FrameRange { next: start, end };
            allocator.range_count += 1;
            allocator.total_frames += (end - start) / PAGE_SIZE;
        }
        allocator
    }

    pub fn allocate(&mut self) -> Option<u64> {
        while self.current_range < self.range_count {
            let range = &mut self.ranges[self.current_range];
            if range.next < range.end {
                let frame = range.next;
                range.next += PAGE_SIZE;
                self.allocated_frames += 1;
                return Some(frame);
            }
            self.current_range += 1;
        }
        None
    }

    #[must_use]
    pub const fn total_frames(&self) -> u64 {
        self.total_frames
    }

    #[must_use]
    pub const fn allocated_frames(&self) -> u64 {
        self.allocated_frames
    }

    #[must_use]
    pub const fn usable_mebibytes(&self) -> u64 {
        self.total_frames * PAGE_SIZE / 1024 / 1024
    }
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.saturating_add(alignment - 1) & !(alignment - 1)
}

const fn align_down(value: u64, alignment: u64) -> u64 {
    value & !(alignment - 1)
}
