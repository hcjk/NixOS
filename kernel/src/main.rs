#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod cpu;
mod framebuffer;
mod gdt;
mod heap;
mod interrupts;
mod memory;
mod monitor;
mod paging;
mod ps2;
mod serial;

use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::panic::PanicInfo;
use limine::request::{FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
    .type _start, @function
_start:
    mov rax, cr0
    and rax, -5
    or rax, 2
    mov cr0, rax
    mov rax, cr4
    or rax, 1536
    mov cr4, rax
    and rsp, -16
    call kernel_main
.Lhalt:
    cli
    hlt
    jmp .Lhalt
    .size _start, . - _start
"#
);

#[unsafe(no_mangle)]
#[allow(clippy::too_many_lines)]
extern "C" fn kernel_main() -> ! {
    let mut serial = serial::SerialPort::new(0x3f8);
    serial.init();
    let _ = writeln!(serial, "\nNexOS 0.3.0-dev x86-64");
    let _ = writeln!(serial, "original Rust kernel; Linux ABI is not used");

    if !BASE_REVISION.is_supported() {
        let _ = writeln!(serial, "fatal: unsupported Limine base revision");
        halt();
    }

    let Some(memory_map) = MEMORY_MAP_REQUEST.response() else {
        let _ = writeln!(serial, "fatal: bootloader supplied no memory map");
        halt();
    };

    let mut allocator = memory::FrameAllocator::from_memory_map(memory_map);
    let bootstrap_frame = allocator.allocate();
    let _ = writeln!(
        serial,
        "memory: {} regions, {} MiB usable, bootstrap frame {:#x}",
        memory_map.entries().len(),
        allocator.usable_mebibytes(),
        bootstrap_frame.unwrap_or(0)
    );

    let Some(hhdm_offset) = HHDM_REQUEST.response().map(|response| response.offset) else {
        let _ = writeln!(
            serial,
            "fatal: bootloader supplied no higher-half direct map"
        );
        halt();
    };
    let Some(mut heap) = heap::KernelHeap::initialize(&mut allocator, hhdm_offset) else {
        let _ = writeln!(serial, "fatal: unable to reserve the kernel heap");
        halt();
    };
    let paging = paging::PagingInfo::detect(hhdm_offset);
    let _ = writeln!(
        serial,
        "paging: CR3 {:#x}, HHDM {:#x}, heap {:#x} ({} KiB)",
        paging.level_4_frame(),
        paging.hhdm_offset(),
        heap.virtual_start(),
        heap.size() / 1024
    );

    let rsdp_address = RSDP_REQUEST
        .response()
        .map(|response| response.address as usize);
    if let Some(address) = rsdp_address {
        let _ = writeln!(serial, "ACPI RSDP: {address:#x}");
    } else {
        let _ = writeln!(serial, "ACPI RSDP: unavailable");
    }

    let framebuffer = FRAMEBUFFER_REQUEST
        .response()
        .and_then(|response| response.framebuffers().first().copied());
    let mut console = framebuffer
        .and_then(framebuffer::Console::from_framebuffer)
        .unwrap_or_else(framebuffer::Console::disabled);

    if let Some(framebuffer) = framebuffer {
        let _ = writeln!(
            serial,
            "framebuffer: {}x{}x{} pitch {}",
            framebuffer.width, framebuffer.height, framebuffer.bpp, framebuffer.pitch
        );
    } else {
        let _ = writeln!(serial, "framebuffer: unavailable; serial fallback active");
    }

    console.clear();
    console.draw_header();
    console.set_color(framebuffer::ACCENT);
    let _ = writeln!(console, "NexOS 0.3.0-dev  |  x86-64 kernel monitor");
    console.set_color(framebuffer::INFO);
    let _ = writeln!(console, "Independent Rust kernel - not based on Linux");
    console.reset_color();
    let _ = writeln!(console);
    let _ = writeln!(
        console,
        "[ok] Limine boot protocol revision accepted\n\
         [ok] {} memory regions, {} MiB usable\n\
         [ok] ACPI RSDP at {:#x}\n\
         [ok] framebuffer {}\n\
         [ok] 256 KiB physical-frame-backed kernel heap",
        memory_map.entries().len(),
        allocator.usable_mebibytes(),
        rsdp_address.unwrap_or(0),
        if framebuffer.is_some() {
            "online"
        } else {
            "serial-only"
        }
    );

    let cpu = cpu::CpuInfo::detect();
    let _ = writeln!(
        serial,
        "cpu: APIC={} NX={} SSE2={}",
        cpu.has_apic, cpu.has_nx, cpu.has_sse2
    );
    interrupts::init();
    let _ = writeln!(
        serial,
        "interrupts: GDT/TSS/IDT online, legacy PIC, PIT 100 Hz, PS/2 IRQ1"
    );
    let _ = writeln!(serial, "milestone 3 ready; entering kernel monitor");
    console.set_color(framebuffer::INFO);
    let _ = writeln!(
        console,
        "[ok] GDT/TSS/IDT, legacy PIC, PIT, and PS/2 IRQ online"
    );
    console.reset_color();

    monitor::Monitor::new(
        &mut console,
        &mut serial,
        &allocator,
        &mut heap,
        &paging,
        &cpu,
        monitor::BootMetadata::new(memory_map.entries().len(), rsdp_address),
    )
    .run()
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut serial = serial::SerialPort::new(0x3f8);
    let _ = writeln!(serial, "\nKERNEL PANIC: {info}");
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: HLT is valid at CPL0 and CLI prevents further interrupt work
        // after a fatal boot or panic condition.
        unsafe {
            asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}
