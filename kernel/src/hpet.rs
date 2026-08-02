use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::acpi::HpetInfo;
use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};

const HPET_VIRTUAL_BASE: u64 = 0xffff_ff80_0000_2000;
const PAGE_MASK: u64 = 0xfff;
const GENERAL_CAPABILITIES: u64 = 0x000;
const GENERAL_CONFIGURATION: u64 = 0x010;
const MAIN_COUNTER: u64 = 0x0f0;
const ENABLE: u64 = 1;
const FEMTOSECONDS_PER_MILLISECOND: u128 = 1_000_000_000_000;

static AVAILABLE: AtomicBool = AtomicBool::new(false);
static REGISTER_BASE: AtomicU64 = AtomicU64::new(0);
static PERIOD_FEMTOSECONDS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct HpetClock {
    pub revision: u8,
    pub timer_count: u8,
    pub counter_64_bit: bool,
    pub period_femtoseconds: u64,
    pub minimum_tick: u16,
}

pub fn initialize(
    info: Option<HpetInfo>,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
) -> Result<HpetClock, HpetError> {
    let info = info.ok_or(HpetError::Unavailable)?;
    if info.address_space != 0 || info.address == 0 {
        return Err(HpetError::UnsupportedAddressSpace);
    }
    let physical_page = info.address & !PAGE_MASK;
    let register_offset = info.address & PAGE_MASK;
    if register_offset > PAGE_MASK.saturating_sub(MAIN_COUNTER + 8) {
        return Err(HpetError::RegistersCrossPage);
    }
    paging.map_mmio_page(HPET_VIRTUAL_BASE, physical_page, allocator)?;
    let register_base = HPET_VIRTUAL_BASE + register_offset;
    let capabilities = read(register_base + GENERAL_CAPABILITIES);
    if capabilities & (1 << 13) == 0 {
        return Err(HpetError::CounterTooNarrow);
    }
    let period_femtoseconds = capabilities >> 32;
    if period_femtoseconds == 0 || period_femtoseconds > 1_000_000_000 {
        return Err(HpetError::InvalidPeriod);
    }
    let configuration = read(register_base + GENERAL_CONFIGURATION);
    write(
        register_base + GENERAL_CONFIGURATION,
        configuration & !ENABLE,
    );
    write(register_base + MAIN_COUNTER, 0);
    write(
        register_base + GENERAL_CONFIGURATION,
        configuration | ENABLE,
    );

    REGISTER_BASE.store(register_base, Ordering::Release);
    PERIOD_FEMTOSECONDS.store(period_femtoseconds, Ordering::Release);
    AVAILABLE.store(true, Ordering::Release);
    Ok(HpetClock {
        revision: capabilities.to_le_bytes()[0],
        timer_count: (capabilities.to_le_bytes()[1] & 0x1f).saturating_add(1),
        counter_64_bit: capabilities & (1 << 13) != 0,
        period_femtoseconds,
        minimum_tick: info.minimum_tick,
    })
}

#[must_use]
pub fn milliseconds() -> Option<u64> {
    if !AVAILABLE.load(Ordering::Acquire) {
        return None;
    }
    let base = REGISTER_BASE.load(Ordering::Acquire);
    let period = PERIOD_FEMTOSECONDS.load(Ordering::Acquire);
    let counter = read(base + MAIN_COUNTER);
    let milliseconds =
        u128::from(counter).saturating_mul(u128::from(period)) / FEMTOSECONDS_PER_MILLISECOND;
    Some(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}

#[must_use]
pub fn available() -> bool {
    AVAILABLE.load(Ordering::Acquire)
}

#[must_use]
pub fn frequency_hz() -> Option<u64> {
    if !available() {
        return None;
    }
    let period = PERIOD_FEMTOSECONDS.load(Ordering::Acquire);
    (period != 0).then_some(1_000_000_000_000_000_u64 / period)
}

fn read(address: u64) -> u64 {
    // SAFETY: Initialization maps the HPET MMIO page uncached and all used
    // registers are naturally aligned 64-bit locations.
    unsafe { core::ptr::read_volatile(address as *const u64) }
}

fn write(address: u64, value: u64) {
    // SAFETY: See `read`; the kernel exclusively configures the main counter.
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
}

#[derive(Clone, Copy, Debug)]
pub enum HpetError {
    Unavailable,
    UnsupportedAddressSpace,
    RegistersCrossPage,
    InvalidPeriod,
    CounterTooNarrow,
    Paging(PageMapError),
}

impl From<PageMapError> for HpetError {
    fn from(error: PageMapError) -> Self {
        Self::Paging(error)
    }
}

impl fmt::Display for HpetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paging(error) => write!(formatter, "paging error: {error}"),
            _ => write!(formatter, "{self:?}"),
        }
    }
}
