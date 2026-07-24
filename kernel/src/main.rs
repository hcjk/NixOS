#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod acpi;
mod ahci;
mod apic;
mod cpu;
mod framebuffer;
mod gdt;
mod heap;
mod ide;
mod interrupts;
mod memory;
mod monitor;
mod paging;
mod pci;
mod ps2;
mod serial;
mod storage;

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
    let _ = writeln!(serial, "\nNexOS 0.6.0-dev x86-64");
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
    let mut paging = paging::PagingInfo::detect(hhdm_offset);
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
    let platform = if let Some(address) = rsdp_address {
        match acpi::PlatformInfo::discover(address, hhdm_offset) {
            Ok(info) => {
                let _ = writeln!(
                    serial,
                    "ACPI: rev {}, {} tables, root={}, CPUs={}, IOAPIC={}, HPET={}, MCFG={}",
                    info.revision,
                    info.table_count,
                    if info.uses_xsdt { "XSDT" } else { "RSDT" },
                    info.enabled_processor_count,
                    info.io_apic.is_some(),
                    info.hpet.is_some(),
                    info.mcfg.is_some()
                );
                Some(info)
            }
            Err(error) => {
                let _ = writeln!(serial, "ACPI discovery failed: {error}");
                None
            }
        }
    } else {
        None
    };
    let pci = pci::PciInventory::scan();
    let _ = writeln!(
        serial,
        "PCI: {} functions discovered{}",
        pci.count(),
        if pci.truncated() { " (truncated)" } else { "" }
    );

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
    let _ = writeln!(console, "NexOS 0.6.0-dev  |  x86-64 kernel monitor");
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
         [ok] 512 KiB reclaiming kernel heap\n\
         [ok] ACPI platform tables and PCI scan",
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
    let interrupt_controller = interrupts::init(platform.as_ref(), &mut paging, &mut allocator);
    let _ = writeln!(
        serial,
        "interrupts: GDT/TSS/IDT online, {}, PIT 100 Hz, PS/2 keyboard=true, mouse={}",
        interrupt_controller.mode.name(),
        interrupt_controller.mouse_enabled
    );
    if let Some(apic) = interrupt_controller.apic {
        let _ = writeln!(
            serial,
            "APIC: local id={} v{:#x}, IOAPIC id={} v{:#x}, redirections={}",
            apic.local_id,
            apic.local_version,
            apic.io_id,
            apic.io_version,
            apic.redirection_entries
        );
    }
    let mut storage = storage::StorageManager::discover(&pci, &mut paging, &mut allocator);
    let _ = writeln!(
        serial,
        "storage: {} disks (AHCI={}, IDE={})",
        storage.count(),
        storage.ahci_count(),
        storage.ide_count()
    );
    let ahci_probe = storage.ahci_probe();
    let _ = writeln!(
        serial,
        "AHCI probe: controllers={}, BARs={}, ABAR={:#x}, mapped={}, map error={:?}, PI ports={}, SATA ports={}, initialized={}, identify failures={}",
        ahci_probe.controllers,
        ahci_probe.bars,
        ahci_probe.last_abar,
        ahci_probe.mapped_controllers,
        ahci_probe.map_error,
        ahci_probe.implemented_ports,
        ahci_probe.sata_ports,
        ahci_probe.initialized_ports,
        ahci_probe.identify_failures
    );
    for index in 0..storage.count() {
        if let Some(device) = storage.device(index) {
            let _ = writeln!(
                serial,
                "disk{index}: {} sectors x {} bytes ({})",
                nexos_storage::BlockDevice::sector_count(device),
                nexos_storage::BlockDevice::sector_size(device),
                device.kind_name()
            );
        }
    }
    let _ = writeln!(serial, "milestone 5 ready; entering kernel monitor");
    console.set_color(framebuffer::INFO);
    let _ = writeln!(
        console,
        "[ok] GDT/TSS/IDT, {}, PIT, keyboard and mouse IRQs",
        interrupt_controller.mode.name()
    );
    let _ = writeln!(
        console,
        "[ok] storage: {} disks (AHCI {}, IDE {})",
        storage.count(),
        storage.ahci_count(),
        storage.ide_count()
    );
    console.reset_color();

    let context = monitor::MonitorContext::new(
        &mut allocator,
        &mut heap,
        &mut paging,
        &cpu,
        platform.as_ref(),
        &pci,
        &mut storage,
        interrupt_controller,
        monitor::BootMetadata::new(memory_map.entries().len(), rsdp_address),
    );
    monitor::Monitor::new(&mut console, &mut serial, context).run()
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
