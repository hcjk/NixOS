use core::arch::asm;
use core::fmt::{self, Write};

use crate::cpu::CpuInfo;
use crate::framebuffer::{ACCENT, Color, Console, FOREGROUND, INFO, MUTED, WARNING};
use crate::memory::FrameAllocator;
use crate::ps2::{self, Keyboard};
use crate::serial::SerialPort;

const MAX_COMMAND_LENGTH: usize = 128;

pub struct Monitor<'a> {
    console: &'a mut Console,
    serial: &'a mut SerialPort,
    keyboard: Keyboard,
    allocator: &'a FrameAllocator,
    cpu: &'a CpuInfo,
    memory_region_count: usize,
    rsdp_address: Option<usize>,
}

impl<'a> Monitor<'a> {
    pub fn new(
        console: &'a mut Console,
        serial: &'a mut SerialPort,
        allocator: &'a FrameAllocator,
        cpu: &'a CpuInfo,
        memory_region_count: usize,
        rsdp_address: Option<usize>,
    ) -> Self {
        Self {
            console,
            serial,
            keyboard: Keyboard::new(),
            allocator,
            cpu,
            memory_region_count,
            rsdp_address,
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
                        byte if byte.is_ascii_graphic() || byte == b' ' => {
                            if length < command.len() {
                                command[length] = byte;
                                length += 1;
                                self.write_both(format_args!("{}", char::from(byte)));
                            }
                        }
                        _ => {}
                    }
                } else {
                    // SAFETY: PAUSE is valid at CPL0.
                    unsafe { asm!("pause", options(nomem, nostack)) };
                }
            }
            self.execute(&command[..length]);
        }
    }

    fn execute(&mut self, command: &[u8]) {
        match command {
            b"" => {}
            b"help" => self.write_line(
                FOREGROUND,
                format_args!(
                    "Commands: help clear uname meminfo cpuinfo bootinfo reboot halt echo"
                ),
            ),
            b"clear" => {
                self.console.clear();
                self.console.draw_header();
                self.console.set_color(ACCENT);
                let _ = self.console.write_str("NexOS kernel monitor\n");
                self.console.reset_color();
            }
            b"uname" => self.write_line(
                FOREGROUND,
                format_args!("NexOS 0.2.0-dev x86_64 (independent kernel)"),
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
