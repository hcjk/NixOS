use core::arch::asm;
use core::fmt::{self, Write};

use crate::cpu::CpuInfo;
use crate::framebuffer::{ACCENT, Color, Console, FOREGROUND, INFO, MUTED, WARNING};
use crate::heap::KernelHeap;
use crate::interrupts;
use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;
use crate::ps2::{self, Keyboard};
use crate::serial::SerialPort;

const MAX_COMMAND_LENGTH: usize = 128;

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
    allocator: &'a FrameAllocator,
    heap: &'a mut KernelHeap,
    paging: &'a PagingInfo,
    cpu: &'a CpuInfo,
    memory_region_count: usize,
    rsdp_address: Option<usize>,
}

impl<'a> Monitor<'a> {
    pub fn new(
        console: &'a mut Console,
        serial: &'a mut SerialPort,
        allocator: &'a FrameAllocator,
        heap: &'a mut KernelHeap,
        paging: &'a PagingInfo,
        cpu: &'a CpuInfo,
        boot: BootMetadata,
    ) -> Self {
        Self {
            console,
            serial,
            keyboard: Keyboard::new(),
            allocator,
            heap,
            paging,
            cpu,
            memory_region_count: boot.memory_region_count,
            rsdp_address: boot.rsdp_address,
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
                        "Commands: help clear uname meminfo heapinfo heaptest cpuinfo bootinfo"
                    ),
                );
                self.write_line(
                    FOREGROUND,
                    format_args!("          uptime virtinfo int3 reboot halt echo"),
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
                format_args!("NexOS 0.3.0-dev x86_64 (independent kernel)"),
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
                    "heap: virt={:#x}, phys={:#x}, used={}/{}, allocations={}",
                    self.heap.virtual_start(),
                    self.heap.physical_start(),
                    self.heap.used(),
                    self.heap.size(),
                    self.heap.allocations()
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
        self.write_line(
            INFO,
            format_args!(
                "heap allocation ok: address={:#x}, size=64, checksum={checksum}",
                allocation.as_ptr() as usize
            ),
        );
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
