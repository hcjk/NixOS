use core::cell::UnsafeCell;

use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 20 * 1024;

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

static TSS: StaticCell<TaskStateSegment> = StaticCell::new(TaskStateSegment::new());
static GDT: StaticCell<GlobalDescriptorTable> = StaticCell::new(GlobalDescriptorTable::new());
static DOUBLE_FAULT_STACK: StaticCell<InterruptStack> =
    StaticCell::new(InterruptStack([0; DOUBLE_FAULT_STACK_SIZE]));

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

        let gdt = &mut *GDT.0.get();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&*TSS.0.get()));

        gdt.load_unsafe();
        CS::set_reg(code_selector);
        SS::set_reg(data_selector);
        DS::set_reg(data_selector);
        ES::set_reg(data_selector);
        load_tss(tss_selector);
    }
}
