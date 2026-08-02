use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU16, Ordering};

use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 20 * 1024;
const PRIVILEGE_STACK_SIZE: usize = 64 * 1024;

struct StaticCell<T>(UnsafeCell<T>);

// SAFETY: These cells are initialized once on the boot CPU before interrupts
// are enabled and never mutated afterwards.
unsafe impl<T> Sync for StaticCell<T> {}

impl<T> StaticCell<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
}

#[repr(align(16))]
struct InterruptStack([u8; DOUBLE_FAULT_STACK_SIZE]);

#[repr(align(16))]
struct PrivilegeStack([u8; PRIVILEGE_STACK_SIZE]);

static TSS: StaticCell<TaskStateSegment> = StaticCell::new(TaskStateSegment::new());
static GDT: StaticCell<GlobalDescriptorTable> = StaticCell::new(GlobalDescriptorTable::new());
static DOUBLE_FAULT_STACK: StaticCell<InterruptStack> =
    StaticCell::new(InterruptStack([0; DOUBLE_FAULT_STACK_SIZE]));
static PRIVILEGE_STACK: StaticCell<PrivilegeStack> =
    StaticCell::new(PrivilegeStack([0; PRIVILEGE_STACK_SIZE]));
static KERNEL_CODE_SELECTOR: AtomicU16 = AtomicU16::new(0);
static KERNEL_DATA_SELECTOR: AtomicU16 = AtomicU16::new(0);
static USER_CODE_SELECTOR: AtomicU16 = AtomicU16::new(0);
static USER_DATA_SELECTOR: AtomicU16 = AtomicU16::new(0);

#[derive(Clone, Copy)]
pub struct Selectors {
    pub kernel_code: u16,
    pub kernel_data: u16,
    pub user_code: u16,
    pub user_data: u16,
}

pub fn init() {
    // SAFETY: This function runs once on the boot CPU before interrupts. The
    // structures have static storage and remain alive for the lifetime of the
    // loaded descriptor registers.
    unsafe {
        let tss = &mut *TSS.0.get();
        let stack = &*DOUBLE_FAULT_STACK.0.get();
        let stack_start = VirtAddr::from_ptr(stack.0.as_ptr());
        tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)] =
            stack_start + DOUBLE_FAULT_STACK_SIZE as u64;
        let privilege_stack = &*PRIVILEGE_STACK.0.get();
        let privilege_stack_start = VirtAddr::from_ptr(privilege_stack.0.as_ptr());
        tss.privilege_stack_table[0] = privilege_stack_start + PRIVILEGE_STACK_SIZE as u64;

        let gdt = &mut *GDT.0.get();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&*TSS.0.get()));

        gdt.load_unsafe();
        CS::set_reg(code_selector);
        SS::set_reg(data_selector);
        DS::set_reg(data_selector);
        ES::set_reg(data_selector);
        load_tss(tss_selector);

        KERNEL_CODE_SELECTOR.store(code_selector.0, Ordering::Release);
        KERNEL_DATA_SELECTOR.store(data_selector.0, Ordering::Release);
        USER_CODE_SELECTOR.store(user_code_selector.0 | 3, Ordering::Release);
        USER_DATA_SELECTOR.store(user_data_selector.0 | 3, Ordering::Release);
    }
}

pub fn load_on_application_processor() {
    let selectors = selectors();
    // SAFETY: The boot CPU completed the immutable shared GDT before starting
    // application processors. APs remain in ring 0 and do not load the shared
    // TSS because each future user-capable CPU requires its own TSS.
    unsafe {
        (&*GDT.0.get()).load_unsafe();
        let code = x86_64::structures::gdt::SegmentSelector(selectors.kernel_code);
        let data = x86_64::structures::gdt::SegmentSelector(selectors.kernel_data);
        CS::set_reg(code);
        SS::set_reg(data);
        DS::set_reg(data);
        ES::set_reg(data);
    }
}

#[must_use]
pub fn selectors() -> Selectors {
    Selectors {
        kernel_code: KERNEL_CODE_SELECTOR.load(Ordering::Acquire),
        kernel_data: KERNEL_DATA_SELECTOR.load(Ordering::Acquire),
        user_code: USER_CODE_SELECTOR.load(Ordering::Acquire),
        user_data: USER_DATA_SELECTOR.load(Ordering::Acquire),
    }
}
