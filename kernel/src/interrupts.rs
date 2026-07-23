use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::serial::SerialPort;
use crate::{gdt, ps2};

const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = 40;
const TIMER_VECTOR: u8 = PIC_1_OFFSET;
const KEYBOARD_VECTOR: u8 = PIC_1_OFFSET + 1;
const PIT_FREQUENCY_HZ: u64 = 100;
const PIT_DIVISOR: u16 = 11_931;

static TICKS: AtomicU64 = AtomicU64::new(0);

struct StaticIdt(UnsafeCell<InterruptDescriptorTable>);

// SAFETY: The IDT is initialized once before interrupts are enabled and is
// subsequently read-only.
unsafe impl Sync for StaticIdt {}

static IDT: StaticIdt = StaticIdt(UnsafeCell::new(InterruptDescriptorTable::new()));

pub fn init() {
    gdt::init();

    // SAFETY: The boot CPU is the only active CPU and interrupts are still
    // disabled, so the static IDT can be initialized without concurrent access.
    unsafe {
        let idt = &mut *IDT.0.get();
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt[TIMER_VECTOR].set_handler_fn(timer_handler);
        idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_handler);
        idt[PIC_1_OFFSET + 7].set_handler_fn(spurious_master_handler);
        idt[PIC_2_OFFSET + 7].set_handler_fn(spurious_slave_handler);
        idt.load_unsafe();

        select_legacy_pic_mode();
        remap_pic();
        configure_pit();
        ps2::enable_keyboard_interrupt();
        asm!("sti", options(nomem, nostack));
    }
}

#[must_use]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

#[must_use]
pub fn uptime_milliseconds() -> u64 {
    ticks().saturating_mul(1000) / PIT_FREQUENCY_HZ
}

pub fn wait_for_interrupt() {
    // SAFETY: The IDT and PIC are initialized before the monitor calls this
    // function. HLT resumes on the next unmasked interrupt.
    unsafe { asm!("hlt", options(nomem, nostack)) };
}

pub fn trigger_breakpoint() {
    // SAFETY: Vector 3 has a present IDT handler and returns to the instruction
    // following INT3.
    unsafe { asm!("int3", options(nomem, nostack)) };
}

extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) {
    fatal_exception("divide error", &frame, None);
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    let mut serial = SerialPort::new(0x3f8);
    let _ = writeln!(
        serial,
        "interrupt: breakpoint handled at {:#x}",
        frame.instruction_pointer.as_u64()
    );
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    fatal_exception("invalid opcode", &frame, None);
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, error_code: u64) -> ! {
    fatal_exception("double fault", &frame, Some(error_code));
}

extern "x86-interrupt" fn general_protection_fault_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    fatal_exception("general protection fault", &frame, Some(error_code));
}

extern "x86-interrupt" fn page_fault_handler(
    frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let cr2: u64;
    // SAFETY: Reading CR2 is valid at CPL0.
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    let mut serial = SerialPort::new(0x3f8);
    let _ = writeln!(
        serial,
        "\nFATAL EXCEPTION: page fault at {cr2:#x}, ip={:#x}, flags={error_code:?}",
        frame.instruction_pointer.as_u64()
    );
    halt_forever()
}

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: IRQ0 came from the master PIC.
    unsafe { end_of_interrupt(0) };
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    // SAFETY: The i8042 status indicates whether a byte can be read. IRQ1 is
    // exclusively routed to this driver.
    unsafe {
        let status = inb(0x64);
        if status & 1 != 0 && status & 0x20 == 0 {
            ps2::enqueue_scancode(inb(0x60));
        }
        end_of_interrupt(1);
    }
}

extern "x86-interrupt" fn spurious_master_handler(_frame: InterruptStackFrame) {
    // A spurious IRQ7 does not require an EOI.
}

extern "x86-interrupt" fn spurious_slave_handler(_frame: InterruptStackFrame) {
    // A spurious IRQ15 only requires an EOI to the master cascade.
    // SAFETY: Writing an EOI to the master PIC is valid here.
    unsafe { outb(0x20, 0x20) };
}

fn fatal_exception(name: &str, frame: &InterruptStackFrame, error_code: Option<u64>) -> ! {
    let mut serial = SerialPort::new(0x3f8);
    let _ = writeln!(
        serial,
        "\nFATAL EXCEPTION: {name}, ip={:#x}, error={:#x}",
        frame.instruction_pointer.as_u64(),
        error_code.unwrap_or(0)
    );
    halt_forever()
}

fn halt_forever() -> ! {
    loop {
        // SAFETY: Disabling interrupts and halting is the terminal exception
        // state until the machine is reset.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

unsafe fn remap_pic() {
    // SAFETY: The boot CPU owns the legacy PIC ports during initialization.
    unsafe {
        let master_mask = inb(0x21);
        let slave_mask = inb(0xa1);

        outb(0x20, 0x11);
        io_wait();
        outb(0xa0, 0x11);
        io_wait();
        outb(0x21, PIC_1_OFFSET);
        io_wait();
        outb(0xa1, PIC_2_OFFSET);
        io_wait();
        outb(0x21, 4);
        io_wait();
        outb(0xa1, 2);
        io_wait();
        outb(0x21, 0x01);
        io_wait();
        outb(0xa1, 0x01);
        io_wait();

        // Enable only the PIT and keyboard on the master; keep the slave
        // masked until a driver needs it.
        let _ = (master_mask, slave_mask);
        outb(0x21, 0b1111_1100);
        outb(0xa1, 0xff);
    }
}

unsafe fn configure_pit() {
    let divisor = PIT_DIVISOR.to_le_bytes();
    // SAFETY: The kernel owns PIT channel 0 and programs square-wave mode.
    unsafe {
        outb(0x43, 0x36);
        outb(0x40, divisor[0]);
        outb(0x40, divisor[1]);
    }
}

unsafe fn end_of_interrupt(irq: u8) {
    // SAFETY: The caller provides the IRQ currently being serviced.
    unsafe {
        if irq >= 8 {
            outb(0xa0, 0x20);
        }
        outb(0x20, 0x20);
    }
}

unsafe fn select_legacy_pic_mode() {
    let low: u32;
    let high: u32;
    // SAFETY: IA32_APIC_BASE is architecturally available when CPUID reports
    // APIC support, which is a NexOS x86-64 platform requirement.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") 0x1b_u32,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
    }
    let apic_base_msr = (u64::from(high) << 32) | u64::from(low);
    let pic_mode_msr = apic_base_msr & !(1 << 11);
    let pic_mode_bytes = pic_mode_msr.to_le_bytes();
    let pic_mode_low = u32::from_le_bytes([
        pic_mode_bytes[0],
        pic_mode_bytes[1],
        pic_mode_bytes[2],
        pic_mode_bytes[3],
    ]);
    let pic_mode_high = u32::from_le_bytes([
        pic_mode_bytes[4],
        pic_mode_bytes[5],
        pic_mode_bytes[6],
        pic_mode_bytes[7],
    ]);
    // SAFETY: Clearing IA32_APIC_BASE.ENABLE selects the architectural legacy
    // PIC delivery path. ACPI/APIC initialization will replace this fallback in
    // the next hardware-discovery milestone.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") 0x1b_u32,
            in("eax") pic_mode_low,
            in("edx") pic_mode_high,
            options(nomem, nostack)
        );
    }
}

unsafe fn io_wait() {
    // SAFETY: Port 0x80 is the traditional POST delay port.
    unsafe { outb(0x80, 0) };
}

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller guarantees ownership and validity of the I/O port.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Caller guarantees ownership and validity of the I/O port.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    }
    value
}
