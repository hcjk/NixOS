use core::arch::asm;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use limine::mp::MpInfo;
use limine::request::MpResponse;

const MAX_CPUS: usize = 64;
const INVALID_ID: u32 = u32::MAX;
const START_TIMEOUT_MILLISECONDS: u64 = 2_000;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuState {
    Absent = 0,
    Starting = 1,
    Online = 2,
}

struct PerCpuState {
    processor_id: AtomicU32,
    lapic_id: AtomicU32,
    state: AtomicU8,
    scheduler_ticks: AtomicU64,
    idle_halts: AtomicU64,
}

impl PerCpuState {
    const fn new() -> Self {
        Self {
            processor_id: AtomicU32::new(INVALID_ID),
            lapic_id: AtomicU32::new(INVALID_ID),
            state: AtomicU8::new(CpuState::Absent as u8),
            scheduler_ticks: AtomicU64::new(0),
            idle_halts: AtomicU64::new(0),
        }
    }
}

static CPUS: [PerCpuState; MAX_CPUS] = [const { PerCpuState::new() }; MAX_CPUS];
static REQUESTED_CPUS: AtomicUsize = AtomicUsize::new(1);
static REGISTERED_CPUS: AtomicUsize = AtomicUsize::new(1);
static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);
static BSP_LAPIC_ID: AtomicU32 = AtomicU32::new(0);
static TRUNCATED: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy)]
pub struct SmpSummary {
    pub requested: usize,
    pub registered: usize,
    pub online: usize,
    pub bsp_lapic_id: u32,
    pub truncated: bool,
}

#[derive(Clone, Copy)]
pub struct CpuSnapshot {
    pub processor_id: u32,
    pub lapic_id: u32,
    pub state: CpuState,
    pub scheduler_ticks: u64,
    pub idle_halts: u64,
}

pub fn initialize(response: Option<&'static MpResponse>) -> SmpSummary {
    let Some(response) = response else {
        configure_slot(0, 0, 0, CpuState::Online);
        return summary();
    };
    let cpus = response.cpus();
    if cpus.is_empty() {
        BSP_LAPIC_ID.store(response.bsp_lapic_id, Ordering::Release);
        configure_slot(0, 0, response.bsp_lapic_id, CpuState::Online);
        return summary();
    }
    let requested = cpus.len().max(1);
    let registered = requested.min(MAX_CPUS);
    REQUESTED_CPUS.store(requested, Ordering::Release);
    REGISTERED_CPUS.store(registered, Ordering::Release);
    BSP_LAPIC_ID.store(response.bsp_lapic_id, Ordering::Release);
    TRUNCATED.store(u8::from(requested > MAX_CPUS), Ordering::Release);
    ONLINE_CPUS.store(0, Ordering::Release);

    for (slot, cpu) in cpus.iter().take(MAX_CPUS).enumerate() {
        let state = if cpu.lapic_id == response.bsp_lapic_id {
            CpuState::Online
        } else {
            CpuState::Starting
        };
        configure_slot(slot, cpu.processor_id, cpu.lapic_id, state);
        if state == CpuState::Online {
            ONLINE_CPUS.fetch_add(1, Ordering::AcqRel);
        }
    }
    for (slot, cpu) in cpus.iter().take(MAX_CPUS).enumerate() {
        if cpu.lapic_id != response.bsp_lapic_id {
            cpu.bootstrap(application_processor_entry, slot as u64);
        }
    }

    let deadline =
        crate::interrupts::uptime_milliseconds().saturating_add(START_TIMEOUT_MILLISECONDS);
    while ONLINE_CPUS.load(Ordering::Acquire) < registered
        && crate::interrupts::uptime_milliseconds() < deadline
    {
        core::hint::spin_loop();
    }
    summary()
}

#[must_use]
pub fn summary() -> SmpSummary {
    SmpSummary {
        requested: REQUESTED_CPUS.load(Ordering::Acquire),
        registered: REGISTERED_CPUS.load(Ordering::Acquire),
        online: ONLINE_CPUS.load(Ordering::Acquire),
        bsp_lapic_id: BSP_LAPIC_ID.load(Ordering::Acquire),
        truncated: TRUNCATED.load(Ordering::Acquire) != 0,
    }
}

#[must_use]
pub fn cpu_snapshot(index: usize) -> Option<CpuSnapshot> {
    if index >= REGISTERED_CPUS.load(Ordering::Acquire) {
        return None;
    }
    let cpu = CPUS.get(index)?;
    let processor_id = cpu.processor_id.load(Ordering::Acquire);
    let lapic_id = cpu.lapic_id.load(Ordering::Acquire);
    Some(CpuSnapshot {
        processor_id,
        lapic_id,
        state: decode_state(cpu.state.load(Ordering::Acquire)),
        scheduler_ticks: cpu.scheduler_ticks.load(Ordering::Relaxed),
        idle_halts: cpu.idle_halts.load(Ordering::Relaxed),
    })
}

pub fn record_scheduler_tick() {
    let bsp_lapic_id = BSP_LAPIC_ID.load(Ordering::Relaxed);
    if let Some(cpu) = CPUS
        .iter()
        .take(REGISTERED_CPUS.load(Ordering::Acquire))
        .find(|cpu| cpu.lapic_id.load(Ordering::Relaxed) == bsp_lapic_id)
    {
        cpu.scheduler_ticks.fetch_add(1, Ordering::Relaxed);
    }
}

fn configure_slot(index: usize, processor_id: u32, lapic_id: u32, state: CpuState) {
    let cpu = &CPUS[index];
    cpu.processor_id.store(processor_id, Ordering::Relaxed);
    cpu.lapic_id.store(lapic_id, Ordering::Relaxed);
    cpu.state.store(state as u8, Ordering::Release);
}

unsafe extern "C" fn application_processor_entry(info: &MpInfo) -> ! {
    let slot = usize::try_from(info.extra_argument()).unwrap_or(MAX_CPUS);
    if slot >= MAX_CPUS {
        halt_forever();
    }
    configure_control_registers();
    crate::gdt::load_on_application_processor();
    crate::interrupts::load_idt_on_application_processor();
    crate::apic::initialize_application_processor();
    CPUS[slot]
        .state
        .store(CpuState::Online as u8, Ordering::Release);
    ONLINE_CPUS.fetch_add(1, Ordering::AcqRel);
    loop {
        CPUS[slot].idle_halts.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The shared IDT and local APIC are active. APs currently own
        // no runnable task, so they sleep until an interrupt or future IPI.
        unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
    }
}

fn configure_control_registers() {
    // SAFETY: Match the BSP's SSE/x87 configuration before Rust code performs
    // any operation that may use those architectural features.
    unsafe {
        asm!(
            "mov rax, cr0",
            "and rax, -5",
            "or rax, 2",
            "mov cr0, rax",
            "mov rax, cr4",
            "or rax, 1536",
            "mov cr4, rax",
            out("rax") _,
            options(nomem, nostack)
        );
    }
}

fn halt_forever() -> ! {
    loop {
        // SAFETY: An invalid AP slot cannot safely enter shared kernel work.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

const fn decode_state(value: u8) -> CpuState {
    match value {
        1 => CpuState::Starting,
        2 => CpuState::Online,
        _ => CpuState::Absent,
    }
}
