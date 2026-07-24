use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nexos_runtime::process::{ProcessId, ProcessState, ProcessTable};
use nexos_runtime::scheduler::{CpuContext, Scheduler, TaskId};
use x86_64::instructions::interrupts;

const MAX_PROCESSES: usize = 16;
const MAX_HANDLES: usize = 16;
const MAX_TASKS: usize = 32;
const QUANTUM_TICKS: u32 = 5;

struct RuntimeCell<T>(UnsafeCell<T>);

// SAFETY: The boot CPU initializes these cells once. All later access occurs
// with interrupts disabled on the single enabled processor.
unsafe impl<T> Sync for RuntimeCell<T> {}

static PROCESS_TABLE: RuntimeCell<ProcessTable<MAX_PROCESSES, MAX_HANDLES>> =
    RuntimeCell(UnsafeCell::new(ProcessTable::new()));
static SCHEDULER: RuntimeCell<Scheduler<MAX_TASKS>> =
    RuntimeCell(UnsafeCell::new(Scheduler::with_quantum(QUANTUM_TICKS)));
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static RESCHEDULE_DECISIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct UserProcess {
    pub process: ProcessId,
    pub task: TaskId,
}

#[derive(Clone, Copy)]
pub struct ProcessSnapshot {
    pub id: ProcessId,
    pub parent: Option<ProcessId>,
    pub state: ProcessState,
    pub entry_point: u64,
}

#[derive(Clone, Copy)]
pub struct SchedulerSnapshot {
    pub tasks: usize,
    pub current: Option<TaskId>,
    pub context_switches: u64,
    pub timer_decisions: u64,
}

pub fn init(address_space: u64, kernel_entry: u64, kernel_stack: u64) -> bool {
    interrupts::without_interrupts(|| {
        if INITIALIZED.load(Ordering::Acquire) {
            return true;
        }
        // SAFETY: The cells have static initialization and are not visible to
        // interrupt or foreground access until INITIALIZED becomes true.
        let processes = unsafe { &mut *PROCESS_TABLE.0.get() };
        // SAFETY: Same one-time boot initialization as PROCESS_TABLE.
        let scheduler = unsafe { &mut *SCHEDULER.0.get() };
        let Ok(kernel_process) = processes.create(None, address_space, kernel_entry, kernel_stack)
        else {
            return false;
        };
        let _ = processes.set_state(kernel_process, ProcessState::Running);

        if scheduler
            .spawn(
                kernel_process.0,
                CpuContext {
                    instruction_pointer: kernel_entry,
                    stack_pointer: kernel_stack,
                    address_space,
                    flags: 0x202,
                    ..CpuContext::default()
                },
            )
            .is_err()
        {
            return false;
        }
        let _ = scheduler.schedule(0);

        INITIALIZED.store(true, Ordering::Release);
        true
    })
}

pub fn on_timer_tick(now: u64) {
    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: Timer interrupts cannot nest on the boot CPU, and all foreground
    // accesses mask interrupts.
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    let before = scheduler.context_switches();
    let _ = scheduler.tick(now);
    if scheduler.context_switches() != before {
        RESCHEDULE_DECISIONS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn create_user_process(
    address_space: u64,
    entry_point: u64,
    stack_pointer: u64,
) -> Option<UserProcess> {
    with_runtime(|processes, scheduler| {
        let process = processes
            .create(None, address_space, entry_point, stack_pointer)
            .ok()?;
        let task = scheduler
            .spawn(
                process.0,
                CpuContext {
                    instruction_pointer: entry_point,
                    stack_pointer,
                    address_space,
                    flags: 0x202,
                    ..CpuContext::default()
                },
            )
            .ok()?;
        Some(UserProcess { process, task })
    })
    .flatten()
}

pub fn finish_user_process(process: UserProcess, status: i32) -> bool {
    with_runtime(|processes, scheduler| {
        processes.exit(process.process, status).is_ok()
            && scheduler.exit_task(process.task, status).is_ok()
    })
    .unwrap_or(false)
}

#[must_use]
pub fn process_count() -> usize {
    with_runtime(|processes, _| processes.len()).unwrap_or(0)
}

#[must_use]
pub fn process_snapshot(index: usize) -> Option<ProcessSnapshot> {
    with_runtime(|processes, _| {
        processes.iter().nth(index).map(|process| ProcessSnapshot {
            id: process.id,
            parent: process.parent,
            state: process.state,
            entry_point: process.entry_point,
        })
    })
    .flatten()
}

#[must_use]
pub fn scheduler_snapshot() -> SchedulerSnapshot {
    with_runtime(|_, scheduler| SchedulerSnapshot {
        tasks: scheduler.task_count(),
        current: scheduler.current().map(|task| task.id),
        context_switches: scheduler.context_switches(),
        timer_decisions: RESCHEDULE_DECISIONS.load(Ordering::Relaxed),
    })
    .unwrap_or(SchedulerSnapshot {
        tasks: 0,
        current: None,
        context_switches: 0,
        timer_decisions: 0,
    })
}

fn with_runtime<R>(
    operation: impl FnOnce(
        &mut ProcessTable<MAX_PROCESSES, MAX_HANDLES>,
        &mut Scheduler<MAX_TASKS>,
    ) -> R,
) -> Option<R> {
    if !INITIALIZED.load(Ordering::Acquire) {
        return None;
    }
    interrupts::without_interrupts(|| {
        // SAFETY: Initialization completed with Release ordering. Interrupts
        // are disabled, and NexOS v0.7 enables only the boot processor.
        let processes = unsafe { &mut *PROCESS_TABLE.0.get() };
        // SAFETY: Same single-CPU critical section as PROCESS_TABLE.
        let scheduler = unsafe { &mut *SCHEDULER.0.get() };
        Some(operation(processes, scheduler))
    })
}
