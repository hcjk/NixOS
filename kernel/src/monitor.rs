use core::arch::asm;
use core::fmt::{self, Write};

use crate::acpi::PlatformInfo;
use crate::cpu::CpuInfo;
use crate::framebuffer::{ACCENT, Color, Console, FOREGROUND, INFO, MUTED, WARNING};
use crate::heap::KernelHeap;
use crate::interrupts;
use crate::interrupts::ControllerInfo;
use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;
use crate::pci::PciInventory;
use crate::ps2::{self, Keyboard, Mouse};
use crate::runtime;
use crate::serial::SerialPort;
use crate::storage::{DeviceLocation, PartitionProbe, StorageManager};
use crate::syscall;
use crate::usb::UsbManager;
use nexos_storage::BlockDevice;

const MAX_COMMAND_LENGTH: usize = 128;
const MAP_TEST_VIRTUAL_ADDRESS: u64 = 0xffff_ff00_0000_0000;

#[derive(Clone, Copy)]
pub struct BootMetadata {
    memory_region_count: usize,
    rsdp_address: Option<usize>,
}

impl BootMetadata {
    #[must_use]
    pub const fn new(memory_region_count: usize, rsdp_address: Option<usize>) -> Self {
        Self {
            memory_region_count,
            rsdp_address,
        }
    }
}

pub struct Monitor<'a> {
    console: &'a mut Console,
    serial: &'a mut SerialPort,
    keyboard: Keyboard,
    mouse: Mouse,
    allocator: &'a mut FrameAllocator,
    heap: &'a mut KernelHeap,
    paging: &'a mut PagingInfo,
    cpu: &'a CpuInfo,
    platform: Option<&'a PlatformInfo>,
    pci: &'a PciInventory,
    usb: &'a mut UsbManager,
    storage: &'a mut StorageManager,
    controller: ControllerInfo,
    memory_region_count: usize,
    rsdp_address: Option<usize>,
}

pub struct MonitorContext<'a> {
    allocator: &'a mut FrameAllocator,
    heap: &'a mut KernelHeap,
    paging: &'a mut PagingInfo,
    cpu: &'a CpuInfo,
    platform: Option<&'a PlatformInfo>,
    pci: &'a PciInventory,
    usb: &'a mut UsbManager,
    storage: &'a mut StorageManager,
    controller: ControllerInfo,
    boot: BootMetadata,
}

impl<'a> MonitorContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        allocator: &'a mut FrameAllocator,
        heap: &'a mut KernelHeap,
        paging: &'a mut PagingInfo,
        cpu: &'a CpuInfo,
        platform: Option<&'a PlatformInfo>,
        pci: &'a PciInventory,
        usb: &'a mut UsbManager,
        storage: &'a mut StorageManager,
        controller: ControllerInfo,
        boot: BootMetadata,
    ) -> Self {
        Self {
            allocator,
            heap,
            paging,
            cpu,
            platform,
            pci,
            usb,
            storage,
            controller,
            boot,
        }
    }
}

impl<'a> Monitor<'a> {
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        console: &'a mut Console,
        serial: &'a mut SerialPort,
        context: MonitorContext<'a>,
    ) -> Self {
        Self {
            console,
            serial,
            keyboard: Keyboard::new(),
            mouse: Mouse::new(),
            allocator: context.allocator,
            heap: context.heap,
            paging: context.paging,
            cpu: context.cpu,
            platform: context.platform,
            pci: context.pci,
            usb: context.usb,
            storage: context.storage,
            controller: context.controller,
            memory_region_count: context.boot.memory_region_count,
            rsdp_address: context.boot.rsdp_address,
        }
    }

    pub fn run(&mut self) -> ! {
        self.write_line(
            INFO,
            format_args!("Interactive PS/2 kernel monitor is ready."),
        );
        self.write_line(MUTED, format_args!("Type 'help' and press Enter."));

        loop {
            self.console.set_color(ACCENT);
            self.write_both(format_args!("nexos> "));
            self.console.reset_color();
            let mut command = [0_u8; MAX_COMMAND_LENGTH];
            let mut length = 0;
            loop {
                if let Some(character) = self.keyboard.read_character() {
                    match character {
                        b'\n' => {
                            self.write_both(format_args!("\n"));
                            break;
                        }
                        8 if length > 0 => {
                            length -= 1;
                            self.console.backspace();
                            let _ = self.serial.write_str("\x08 \x08");
                        }
                        byte if (byte.is_ascii_graphic() || byte == b' ')
                            && length < command.len() =>
                        {
                            command[length] = byte;
                            length += 1;
                            self.write_both(format_args!("{}", char::from(byte)));
                        }
                        _ => {}
                    }
                } else {
                    interrupts::wait_for_interrupt();
                }
            }
            self.execute(&command[..length]);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute(&mut self, command: &[u8]) {
        match command {
            b"" => {}
            b"help" => {
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "Commands: help clear uname meminfo heapinfo heapstats heaptest cpuinfo"
                    ),
                );
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "          bootinfo acpi lspci lsusb usbinfo usbtest lsblk disktest"
                    ),
                );
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "          irqinfo mouseinfo uptime virtinfo maptest ps schedinfo syscalls"
                    ),
                );
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "          usertest vfspath commands shellparse shelltest int3 reboot halt echo"
                    ),
                );
            }
            b"clear" => {
                self.console.clear();
                self.console.draw_header();
                self.console.set_color(ACCENT);
                let _ = self.console.write_str("NexOS kernel monitor\n");
                self.console.reset_color();
            }
            b"uname" => self.write_line(
                FOREGROUND,
                format_args!("NexOS 0.9.0-dev x86_64 (independent kernel)"),
            ),
            b"meminfo" => self.write_line(
                FOREGROUND,
                format_args!(
                    "usable: {} MiB, frames: {}, allocated: {}",
                    self.allocator.usable_mebibytes(),
                    self.allocator.total_frames(),
                    self.allocator.allocated_frames()
                ),
            ),
            b"heapinfo" => self.write_line(
                FOREGROUND,
                format_args!(
                    "heap: used={}/{}, free={}, largest={}, active={}, peak={}",
                    self.heap.used(),
                    self.heap.size(),
                    self.heap.free_bytes(),
                    self.heap.largest_free_block(),
                    self.heap.active_allocations(),
                    self.heap.peak_used()
                ),
            ),
            b"heapstats" => self.write_line(
                FOREGROUND,
                format_args!(
                    "heap: virt={:#x}, phys={:#x}, allocs={}, frees={}",
                    self.heap.virtual_start(),
                    self.heap.physical_start(),
                    self.heap.total_allocations(),
                    self.heap.deallocations()
                ),
            ),
            b"heaptest" => self.test_heap(),
            b"cpuinfo" => self.write_line(
                FOREGROUND,
                format_args!(
                    "x86_64 features: APIC={} NX={} SSE2={}",
                    yes_no(self.cpu.has_apic),
                    yes_no(self.cpu.has_nx),
                    yes_no(self.cpu.has_sse2)
                ),
            ),
            b"bootinfo" => {
                let region_count = self.memory_region_count;
                let rsdp_address = self.rsdp_address.unwrap_or(0);
                self.write_line(
                    FOREGROUND,
                    format_args!("Limine memory regions: {region_count}, RSDP: {rsdp_address:#x}"),
                );
            }
            b"acpi" => self.print_acpi(),
            b"lspci" => self.print_pci(),
            b"lsusb" => self.print_usb_devices(),
            b"usbinfo" => self.print_usb_info(),
            b"usbtest" => self.test_usb(),
            b"lsblk" => self.print_storage(),
            _ if command.starts_with(b"disktest ") => {
                if let Some(index) = parse_decimal(&command[9..]) {
                    self.test_disk(index);
                } else {
                    self.write_line(WARNING, format_args!("usage: disktest <disk-number>"));
                }
            }
            b"irqinfo" => self.print_interrupts(),
            b"mouseinfo" => self.print_mouse(),
            b"uptime" => {
                let milliseconds = interrupts::uptime_milliseconds();
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "uptime: {}.{:03} seconds ({} PIT ticks)",
                        milliseconds / 1000,
                        milliseconds % 1000,
                        interrupts::ticks()
                    ),
                );
            }
            b"virtinfo" => {
                let kernel_address = crate::kernel_main as *const () as usize as u64;
                let mapping = self.paging.translate(kernel_address);
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "CR3={:#x}, HHDM={:#x}",
                        self.paging.level_4_frame(),
                        self.paging.hhdm_offset()
                    ),
                );
                if let Some(mapping) = mapping {
                    self.write_line(
                        FOREGROUND,
                        format_args!(
                            "kernel virt {kernel_address:#x} -> phys {:#x}, page={} KiB, flags={:#x}",
                            mapping.physical_address,
                            mapping.page_size / 1024,
                            mapping.flags
                        ),
                    );
                } else {
                    self.write_line(WARNING, format_args!("kernel mapping was not found"));
                }
            }
            b"maptest" => self.test_mapping(),
            b"ps" => self.print_processes(),
            b"schedinfo" => {
                let scheduler = runtime::scheduler_snapshot();
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "scheduler: tasks={}, current={:?}, switches={}, timer decisions={}, quantum=5",
                        scheduler.tasks,
                        scheduler.current,
                        scheduler.context_switches,
                        scheduler.timer_decisions
                    ),
                );
            }
            b"syscalls" => {
                let (count, last, exit_status) = syscall::statistics();
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "syscall ABI v{}, calls={count}, last={last:?}, last exit={exit_status}",
                        nexos_abi::ABI_VERSION
                    ),
                );
            }
            b"usertest" => self.test_user_mode(),
            _ if command.starts_with(b"vfspath ") => {
                match nexos_runtime::vfs::Path::normalize(&command[8..], b"/") {
                    Ok(path) => self.write_line(
                        INFO,
                        format_args!("VFS normalized path: {}", Ascii(path.as_bytes())),
                    ),
                    Err(error) => {
                        self.write_line(WARNING, format_args!("VFS path rejected: {error:?}"))
                    }
                }
            }
            b"commands" => self.print_userspace_commands(),
            _ if command.starts_with(b"shellparse ") => {
                self.parse_shell_line(&command[11..]);
            }
            b"shelltest" => self.parse_shell_line(b"echo \"$HOME\" | wc -c > /tmp/count"),
            b"int3" => {
                self.write_line(MUTED, format_args!("Triggering breakpoint interrupt..."));
                interrupts::trigger_breakpoint();
                self.write_line(
                    INFO,
                    format_args!("Breakpoint handler returned successfully."),
                );
            }
            b"reboot" => {
                self.write_line(WARNING, format_args!("Rebooting through i8042..."));
                ps2::reboot();
            }
            b"halt" => {
                self.write_line(
                    WARNING,
                    format_args!("CPU halted. Reset the VM to continue."),
                );
                loop {
                    // SAFETY: HLT is valid at CPL0.
                    unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
                }
            }
            _ if command.starts_with(b"echo ") => {
                let text = &command[5..];
                for byte in text {
                    self.write_both(format_args!("{}", char::from(*byte)));
                }
                self.write_both(format_args!("\n"));
            }
            _ => self.write_line(
                WARNING,
                format_args!("Unknown command '{}'. Type 'help'.", Ascii(command)),
            ),
        }
    }

    fn test_heap(&mut self) {
        let Some(allocation) = self.heap.allocate(64, 16) else {
            self.write_line(WARNING, format_args!("heap allocation failed"));
            return;
        };
        let mut checksum = 0_u64;
        for index in 0_u8..64 {
            let value = index.wrapping_mul(3).wrapping_add(1);
            // SAFETY: The heap returned a live 64-byte allocation and index is
            // restricted to that allocation.
            unsafe { allocation.as_ptr().add(usize::from(index)).write(value) };
            checksum += u64::from(value);
        }
        let first_address = allocation.as_ptr() as usize;
        let released = self.heap.deallocate(allocation);
        let reused = if let Some(second) = self.heap.allocate(64, 16) {
            let same_address = second.as_ptr() as usize == first_address;
            let _ = self.heap.deallocate(second);
            same_address
        } else {
            false
        };
        self.write_line(
            INFO,
            format_args!(
                "heap ok: address={first_address:#x}, checksum={checksum}, released={released}, reused={reused}"
            ),
        );
    }

    fn test_mapping(&mut self) {
        let Some(frame) = self.allocator.allocate() else {
            self.write_line(WARNING, format_args!("maptest: no free physical frame"));
            return;
        };
        let result = self
            .paging
            .map_writable_page(MAP_TEST_VIRTUAL_ADDRESS, frame, self.allocator);
        if let Err(error) = result {
            self.write_line(WARNING, format_args!("maptest: map failed: {error}"));
            return;
        }
        let expected = 0x4e45_584f_534d_4150_u64;
        // SAFETY: The test virtual page was just mapped writable to an
        // exclusively allocated frame.
        unsafe {
            core::ptr::write_volatile(MAP_TEST_VIRTUAL_ADDRESS as *mut u64, expected);
        }
        // SAFETY: The same live test mapping remains present.
        let observed = unsafe { core::ptr::read_volatile(MAP_TEST_VIRTUAL_ADDRESS as *const u64) };
        let translated = self
            .paging
            .translate(MAP_TEST_VIRTUAL_ADDRESS)
            .map(|mapping| mapping.physical_address);
        let unmapped = self.paging.unmap_page(MAP_TEST_VIRTUAL_ADDRESS);
        let unmapped_expected = matches!(unmapped, Ok(value) if value == frame);
        let passed = observed == expected && translated == Some(frame) && unmapped_expected;
        self.write_line(
            if passed { INFO } else { WARNING },
            format_args!(
                "maptest: phys={frame:#x}, translated={:#x}, value={observed:#x}, unmapped={}, passed={passed}",
                translated.unwrap_or(0),
                unmapped.is_ok()
            ),
        );
    }

    fn test_user_mode(&mut self) {
        let Some(process) = runtime::create_user_process(
            self.paging.level_4_frame(),
            0x0000_0000_4000_0000,
            0x0000_0000_7fff_1000,
        ) else {
            self.write_line(WARNING, format_args!("usertest: process table is full"));
            return;
        };
        self.write_line(
            MUTED,
            format_args!(
                "usertest: entering ring 3 as pid {} through IRETQ",
                process.process.0
            ),
        );
        let result = syscall::run_ring3_self_test(self.paging, self.allocator);
        let status = result.unwrap_or(-1);
        let finished = runtime::finish_user_process(process, status as i32);
        match result {
            Ok(value) if value == i64::from(nexos_abi::ABI_VERSION) && finished => {
                self.write_line(
                    INFO,
                    format_args!(
                        "usertest passed: ring 3 -> SYSCALL -> ABI v{value} -> Exit -> ring 0"
                    ),
                );
            }
            Ok(value) => self.write_line(
                WARNING,
                format_args!("usertest returned unexpected status {value}, finalized={finished}"),
            ),
            Err(error) => {
                self.write_line(WARNING, format_args!("usertest setup failed: {error:?}"))
            }
        }
    }

    fn print_processes(&mut self) {
        let count = runtime::process_count();
        self.write_line(
            FOREGROUND,
            format_args!("PID  PPID  STATE                 ENTRY"),
        );
        for index in 0..count {
            let Some(process) = runtime::process_snapshot(index) else {
                continue;
            };
            self.write_line(
                FOREGROUND,
                format_args!(
                    "{:<4} {:<5} {:<21} {:#x}",
                    process.id.0,
                    process.parent.map_or(0, |parent| parent.0),
                    process.state,
                    process.entry_point
                ),
            );
        }
    }

    fn print_userspace_commands(&mut self) {
        let commands = nexos_userspace::commands::COMMANDS;
        self.write_line(
            FOREGROUND,
            format_args!("userspace command registry: {} commands", commands.len()),
        );
        for command in commands {
            self.write_line(
                FOREGROUND,
                format_args!(
                    "{:<12} {:?}: {}",
                    command.name, command.class, command.summary
                ),
            );
        }
    }

    fn parse_shell_line(&mut self, line: &[u8]) {
        let mut environment = nexos_userspace::shell::Environment::new();
        let _ = environment.set(b"HOME", b"/home/root");
        let parsed = match nexos_userspace::shell::parse(line, &environment) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.write_line(WARNING, format_args!("shell parse error: {error:?}"));
                return;
            }
        };
        self.write_line(
            INFO,
            format_args!(
                "shell syntax valid: {} pipeline stage(s)",
                parsed.command_count()
            ),
        );
        for stage_index in 0..parsed.command_count() {
            let Some(stage) = parsed.command(stage_index) else {
                continue;
            };
            self.write_both(format_args!("  stage {}:", stage_index + 1));
            for argument_index in 0..stage.argument_count() {
                let Some(span) = stage.argument(argument_index) else {
                    continue;
                };
                self.write_both(format_args!(" [{}]", Ascii(parsed.bytes(span))));
            }
            if let Some(input) = stage.input {
                self.write_both(format_args!(" < {}", Ascii(parsed.bytes(input))));
            }
            if let Some(output) = stage.output {
                self.write_both(format_args!(
                    " {} {}",
                    if output.append { ">>" } else { ">" },
                    Ascii(parsed.bytes(output.path))
                ));
            }
            self.write_both(format_args!("\n"));
        }
    }

    fn print_acpi(&mut self) {
        let Some(platform) = self.platform else {
            self.write_line(WARNING, format_args!("ACPI platform data unavailable"));
            return;
        };
        self.write_line(
            FOREGROUND,
            format_args!(
                "ACPI rev {}, root={}, tables={}, CPUs={}, ISOs={}",
                platform.revision,
                if platform.uses_xsdt { "XSDT" } else { "RSDT" },
                platform.table_count,
                platform.enabled_processor_count,
                platform.interrupt_override_count()
            ),
        );
        let local_apic = platform.local_apic_address.unwrap_or(0);
        let (io_id, io_apic, global_base) = platform.io_apic.map_or((0, 0, 0), |info| {
            (info.id, info.address, info.global_interrupt_base)
        });
        self.write_line(
            FOREGROUND,
            format_args!(
                "MADT: LAPIC={local_apic:#x}, IOAPIC id={io_id} at {io_apic:#x}, GSI base={global_base}"
            ),
        );
        if let Some(hpet) = platform.hpet {
            self.write_line(
                FOREGROUND,
                format_args!(
                    "HPET: space={}, address={:#x}, minimum tick={}",
                    hpet.address_space, hpet.address, hpet.minimum_tick
                ),
            );
        }
        if let Some(mcfg) = platform.mcfg {
            self.write_line(
                FOREGROUND,
                format_args!(
                    "MCFG: base={:#x}, segment={}, buses={}-{}",
                    mcfg.base_address, mcfg.segment_group, mcfg.start_bus, mcfg.end_bus
                ),
            );
        }
    }

    fn print_pci(&mut self) {
        let count = self.pci.count();
        self.write_line(
            FOREGROUND,
            format_args!(
                "PCI functions: {count}{}",
                if self.pci.truncated() {
                    " (inventory truncated)"
                } else {
                    ""
                }
            ),
        );
        for index in 0..count {
            let device = self.pci.devices()[index];
            self.write_line(
                FOREGROUND,
                format_args!(
                    "{:02x}:{:02x}.{} {:04x}:{:04x} rev {:02x} class {:02x}:{:02x}:{:02x} {}",
                    device.bus,
                    device.device,
                    device.function,
                    device.vendor_id,
                    device.device_id,
                    device.revision,
                    device.class,
                    device.subclass,
                    device.programming_interface,
                    device.class_name()
                ),
            );
        }
    }

    fn print_usb_devices(&mut self) {
        self.usb.refresh();
        let stats = self.usb.stats();
        self.write_line(
            FOREGROUND,
            format_args!(
                "USB root devices: {} connected on {} xHCI controller(s)",
                stats.connected_ports,
                self.usb.controller_count()
            ),
        );
        let mut found = false;
        for controller_index in 0..self.usb.controller_count() {
            let Some(controller) = self.usb.controller(controller_index).copied() else {
                continue;
            };
            for device in controller.devices().iter().copied() {
                found = true;
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "Bus {:02} Device {:03}: ID {:04x}:{:04x} USB {}.{:02} slot={} port={} speed={} class={:02x}:{:02x}:{:02x}",
                        controller_index + 1,
                        device.address,
                        device.vendor_id,
                        device.product_id,
                        device.usb_version >> 8,
                        device.usb_version & 0xff,
                        device.slot_id,
                        device.root_port,
                        usb_speed_name(device.speed),
                        device.device_class,
                        device.device_subclass,
                        device.device_protocol
                    ),
                );
                self.write_line(
                    MUTED,
                    format_args!(
                        "  config={} interfaces={} endpoints={} HID-keyboard={} HID-mouse={} hubs={} mass-storage={}",
                        device.configuration_value,
                        device.summary.interfaces,
                        device.summary.endpoints,
                        device.summary.hid_keyboards,
                        device.summary.hid_mice,
                        device.summary.hubs,
                        device.summary.mass_storage
                    ),
                );
            }
            for port in controller.ports().iter().copied() {
                if !port.connected
                    || controller
                        .devices()
                        .iter()
                        .any(|device| device.root_port == port.number)
                {
                    continue;
                }
                found = true;
                self.write_line(
                    FOREGROUND,
                    format_args!(
                        "Bus {:02} Port {:02}: speed={}, enabled={}, powered={}, link-state={}",
                        controller_index + 1,
                        port.number,
                        usb_speed_name(port.speed),
                        yes_no(port.enabled),
                        yes_no(port.powered),
                        port.link_state
                    ),
                );
            }
        }
        if !found {
            self.write_line(MUTED, format_args!("No connected USB root-port devices."));
        }
    }

    fn print_usb_info(&mut self) {
        self.usb.refresh();
        let stats = self.usb.stats();
        self.write_line(
            FOREGROUND,
            format_args!(
                "xHCI: PCI={}, initialized={}, ports={}, connected={}, enabled={}, enumerated={}/{}, failures={}, commands={}",
                stats.pci_controllers,
                stats.initialized_controllers,
                stats.total_ports,
                stats.connected_ports,
                stats.enabled_ports,
                stats.enumerated_devices,
                stats.enumeration_attempts,
                stats.enumeration_failures,
                stats.command_completions
            ),
        );
        for index in 0..self.usb.controller_count() {
            let Some(controller) = self.usb.controller(index).copied() else {
                continue;
            };
            self.write_line(
                FOREGROUND,
                format_args!(
                    "xhci{index}: {:02x}:{:02x}.{} {:04x}:{:04x}, HCIVERSION={}.{:02}, slots={}, interrupters={}, context={}B, AC64={}, scratchpads={}, no-op={}",
                    controller.bus,
                    controller.device,
                    controller.function,
                    controller.vendor_id,
                    controller.device_id,
                    controller.version >> 8,
                    controller.version & 0xff,
                    controller.max_slots,
                    controller.max_interrupters,
                    controller.context_bytes,
                    yes_no(controller.supports_64_bit),
                    controller.scratchpads,
                    controller.no_op_completion
                ),
            );
        }
        if let Some(error) = stats.last_error {
            self.write_line(
                WARNING,
                format_args!(
                    "last xHCI error: {error:?} (detail={})",
                    error.detail_code()
                ),
            );
        }
    }

    fn test_usb(&mut self) {
        let passed = self.usb.self_test();
        let stats = self.usb.stats();
        self.write_line(
            if passed { INFO } else { WARNING },
            format_args!(
                "usbtest: controllers={}, commands={}, connected={}, completion=success, passed={passed}",
                stats.initialized_controllers,
                stats.command_completions,
                stats.connected_ports
            ),
        );
        if let Some(error) = stats.last_error {
            self.write_line(
                WARNING,
                format_args!("usbtest error: {error:?} (detail={})", error.detail_code()),
            );
        }
    }

    fn print_interrupts(&mut self) {
        let mode = self.controller.mode.name();
        self.write_line(
            FOREGROUND,
            format_args!(
                "interrupt controller: {mode}, PIT={} Hz, mouse={}",
                100,
                yes_no(self.controller.mouse_enabled)
            ),
        );
        if let Some(apic) = self.controller.apic {
            self.write_line(
                FOREGROUND,
                format_args!(
                    "LAPIC id={} version={:#x}; IOAPIC id={} version={:#x}, entries={}",
                    apic.local_id,
                    apic.local_version,
                    apic.io_id,
                    apic.io_version,
                    apic.redirection_entries
                ),
            );
        }
    }

    fn print_storage(&mut self) {
        let count = self.storage.count();
        self.write_line(
            FOREGROUND,
            format_args!(
                "block devices: {count} (AHCI={}, IDE={})",
                self.storage.ahci_count(),
                self.storage.ide_count()
            ),
        );
        for index in 0..count {
            let Some(device) = self.storage.device(index) else {
                continue;
            };
            let sectors = device.sector_count();
            let sector_size = device.sector_size();
            let capacity_mib = sectors.saturating_mul(u64::from(sector_size)) / 1024 / 1024;
            let kind = device.kind_name();
            let location = device.location();
            let model_bytes = device.model_bytes();
            let mut model = [0_u8; 40];
            let model_length = model_bytes.len().min(model.len());
            model[..model_length].copy_from_slice(&model_bytes[..model_length]);
            match location {
                DeviceLocation::AhciPort { port, version } => self.write_line(
                    FOREGROUND,
                    format_args!(
                        "disk{index}: {kind}, AHCI {version:#010x} port {port}, {capacity_mib} MiB, {sector_size}-byte sectors, {}",
                        Ascii(&model[..model_length])
                    ),
                ),
                DeviceLocation::IdePosition { slave } => self.write_line(
                    FOREGROUND,
                    format_args!(
                        "disk{index}: {kind}, IDE {}, {capacity_mib} MiB, {sector_size}-byte sectors, {}",
                        if slave { "slave" } else { "master" },
                        Ascii(&model[..model_length])
                    ),
                ),
            }
            match self.storage.probe_partitions(index) {
                PartitionProbe::None => {
                    self.write_line(MUTED, format_args!("  no MBR/GPT partition table"));
                }
                PartitionProbe::Mbr { entries } => {
                    let partitions = entries.iter().flatten().count();
                    self.write_line(
                        FOREGROUND,
                        format_args!("  MBR: {partitions} primary partitions"),
                    );
                    for (partition_index, partition) in entries.iter().enumerate() {
                        if let Some(partition) = partition {
                            self.write_line(
                                FOREGROUND,
                                format_args!(
                                    "    p{}: type={:#04x}, first={}, sectors={}, boot={}",
                                    partition_index + 1,
                                    partition.partition_type,
                                    partition.first_lba,
                                    partition.sector_count,
                                    yes_no(partition.bootable)
                                ),
                            );
                        }
                    }
                }
                PartitionProbe::Gpt {
                    entry_count,
                    first_usable_lba,
                    last_usable_lba,
                } => self.write_line(
                    FOREGROUND,
                    format_args!(
                        "  GPT valid: {entry_count} slots, usable LBAs {first_usable_lba}-{last_usable_lba}"
                    ),
                ),
                PartitionProbe::Invalid => {
                    self.write_line(WARNING, format_args!("  invalid partition metadata"));
                }
                PartitionProbe::ReadError(error) => {
                    self.write_line(WARNING, format_args!("  partition read failed: {error:?}"));
                }
            }
        }
    }

    fn test_disk(&mut self, index: usize) {
        let Some(device) = self.storage.device_mut(index) else {
            self.write_line(WARNING, format_args!("disk{index} does not exist"));
            return;
        };
        if device.sector_size() != 512 {
            self.write_line(
                WARNING,
                format_args!("disktest currently supports 512-byte sectors"),
            );
            return;
        }
        let mut sector = [0_u8; 512];
        let result = device.read_sectors(0, &mut sector);
        match result {
            Ok(()) => {
                let checksum = nexos_storage::crc32(&sector);
                let signature = u16::from_le_bytes([sector[510], sector[511]]);
                self.write_line(
                    INFO,
                    format_args!(
                        "disk{index} read-only test passed: LBA0 CRC32={checksum:#010x}, signature={signature:#06x}"
                    ),
                );
            }
            Err(error) => self.write_line(
                WARNING,
                format_args!("disk{index} read-only test failed: {error:?}"),
            ),
        }
    }

    fn print_mouse(&mut self) {
        self.mouse.drain();
        let total = self.mouse.total_events();
        if let Some(event) = self.mouse.latest() {
            self.write_line(
                FOREGROUND,
                format_args!(
                    "mouse events={total}, last dx={}, dy={}, buttons={:#05b}",
                    event.delta_x, event.delta_y, event.buttons
                ),
            );
        } else {
            self.write_line(
                MUTED,
                format_args!("mouse events=0; move or click the PS/2 mouse, then retry"),
            );
        }
    }

    fn write_line(&mut self, color: Color, arguments: fmt::Arguments<'_>) {
        self.console.set_color(color);
        self.write_both(arguments);
        self.write_both(format_args!("\n"));
        self.console.reset_color();
    }

    fn write_both(&mut self, arguments: fmt::Arguments<'_>) {
        let _ = self.console.write_fmt(arguments);
        let _ = self.serial.write_fmt(arguments);
    }
}

struct Ascii<'a>(&'a [u8]);

impl fmt::Display for Ascii<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            formatter.write_char(char::from(*byte))?;
        }
        Ok(())
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

const fn usb_speed_name(speed: nexos_usb::UsbSpeed) -> &'static str {
    match speed {
        nexos_usb::UsbSpeed::Low => "low-speed",
        nexos_usb::UsbSpeed::Full => "full-speed",
        nexos_usb::UsbSpeed::High => "high-speed",
        nexos_usb::UsbSpeed::Super => "SuperSpeed",
        nexos_usb::UsbSpeed::SuperPlus => "SuperSpeedPlus",
        nexos_usb::UsbSpeed::Unknown => "unknown",
    }
}

fn parse_decimal(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_usize;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(usize::from(*byte - b'0'))?;
    }
    Some(value)
}
