#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod freestanding {
    use core::panic::PanicInfo;

    use nexos_abi::{DirectoryEntry, Error, FileStat, FileType};
    use nexos_userspace::runtime;

    const MAX_LINE: usize = 256;

    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text._start")]
    pub extern "C" fn _start() -> ! {
        write_all(b"NexOS filesystem userspace ready (ABI v");
        match runtime::abi_version() {
            Ok(version) => write_u64(u64::from(version)),
            Err(_) => write_all(b"?"),
        }
        write_all(b")\nType 'help' for commands.\n");
        shell_loop()
    }

    fn shell_loop() -> ! {
        let mut line = [0_u8; MAX_LINE];
        loop {
            write_all(b"nexsh> ");
            let length = read_line(&mut line);
            if execute(&line[..length]) {
                runtime::exit(0);
            }
        }
    }

    fn read_line(line: &mut [u8]) -> usize {
        let mut length = 0;
        loop {
            let mut byte = [0_u8; 1];
            match runtime::read(0, &mut byte) {
                Ok(0) | Err(Error::WouldBlock) => continue,
                Err(_) => runtime::exit(1),
                Ok(_) => {}
            }
            match byte[0] {
                b'\n' | b'\r' => {
                    write_all(b"\n");
                    return length;
                }
                3 => {
                    write_all(b"^C\n");
                    return 0;
                }
                8 | 0x7f if length != 0 => {
                    length -= 1;
                    write_all(b"\x08 \x08");
                }
                byte if (byte.is_ascii_graphic() || byte == b' ') && length < line.len() => {
                    line[length] = byte;
                    length += 1;
                    write_all(&[byte]);
                }
                _ => {}
            }
        }
    }

    fn execute(line: &[u8]) -> bool {
        let line = trim(line);
        if line.is_empty() {
            return false;
        }
        let (command, arguments) = split_once(line);
        match command {
            b"help" => write_all(
                b"Commands: help uname smpinfo uptime pwd echo ls cat stat clear shutdown reboot exit\n\
                  Programs and system files are loaded from the mounted NexFS root.\n",
            ),
            b"uname" => write_all(b"NexOS 0.15.0-dev x86_64 (ring-3 userspace)\n"),
            b"smpinfo" => {
                write_all(b"processors: registered=");
                write_u64(runtime::processor_count().unwrap_or(0));
                write_all(b" online=");
                write_u64(runtime::online_processor_count().unwrap_or(0));
                write_all(b"\n");
            }
            b"uptime" => {
                let milliseconds = runtime::uptime_milliseconds().unwrap_or(0);
                write_all(b"uptime: ");
                write_u64(milliseconds / 1000);
                write_all(b".");
                write_padded_milliseconds(milliseconds % 1000);
                write_all(b" seconds\n");
            }
            b"pwd" => write_all(b"/\n"),
            b"echo" => {
                write_all(arguments);
                write_all(b"\n");
            }
            b"ls" => list(if arguments.is_empty() {
                b"/"
            } else {
                arguments
            }),
            b"cat" => {
                if arguments.is_empty() {
                    write_all(b"usage: cat <path>\n");
                } else {
                    cat(arguments);
                }
            }
            b"stat" => {
                if arguments.is_empty() {
                    write_all(b"usage: stat <path>\n");
                } else {
                    stat(arguments);
                }
            }
            b"clear" => write_all(b"\x1b[2J\x1b[H"),
            b"shutdown" => {
                write_all(b"Requesting ACPI shutdown...\n");
                if runtime::shutdown().is_err() {
                    write_all(b"shutdown: ACPI power-off failed\n");
                }
            }
            b"reboot" => {
                write_all(b"Requesting ACPI reset...\n");
                if runtime::reboot().is_err() {
                    write_all(b"reboot: reset failed\n");
                }
            }
            b"exit" => return true,
            _ => write_all(b"command not found; type 'help'\n"),
        }
        false
    }

    fn list(path: &[u8]) {
        let Ok(handle) = runtime::open(path, nexos_abi::OPEN_DIRECTORY) else {
            write_all(b"ls: cannot open directory\n");
            return;
        };
        loop {
            let mut entry = DirectoryEntry::default();
            match runtime::read_dir(handle, &mut entry) {
                Ok(false) => break,
                Ok(true) => {
                    let length = usize::from(entry.name_length).min(entry.name.len());
                    write_all(&entry.name[..length]);
                    if entry.file_type == FileType::Directory as u32 {
                        write_all(b"/");
                    }
                    write_all(b"  ");
                }
                Err(_) => {
                    write_all(b"ls: directory read failed");
                    break;
                }
            }
        }
        write_all(b"\n");
        let _ = runtime::close(handle);
    }

    fn cat(path: &[u8]) {
        let Ok(handle) = runtime::open(path, 0) else {
            write_all(b"cat: file not found\n");
            return;
        };
        let mut buffer = [0_u8; 256];
        loop {
            match runtime::read(handle, &mut buffer) {
                Ok(0) => break,
                Ok(length) => write_all(&buffer[..length]),
                Err(_) => {
                    write_all(b"cat: read failed\n");
                    break;
                }
            }
        }
        let _ = runtime::close(handle);
    }

    fn stat(path: &[u8]) {
        let mut stat = FileStat::default();
        if runtime::stat(path, &mut stat).is_err() {
            write_all(b"stat: path not found\n");
            return;
        }
        write_all(b"inode=");
        write_u64(stat.inode);
        write_all(b" size=");
        write_u64(stat.size);
        write_all(b" type=");
        write_u64(u64::from(stat.file_type));
        write_all(b" mode=");
        write_octal(stat.permissions);
        write_all(b"\n");
    }

    fn write_all(bytes: &[u8]) {
        let mut completed = 0;
        while completed < bytes.len() {
            match runtime::write(1, &bytes[completed..]) {
                Ok(0) | Err(_) => return,
                Ok(amount) => completed += amount,
            }
        }
    }

    fn write_u64(mut value: u64) {
        let mut output = [0_u8; 20];
        let mut index = output.len();
        if value == 0 {
            write_all(b"0");
            return;
        }
        while value != 0 {
            index -= 1;
            output[index] = b'0' + u8::try_from(value % 10).unwrap_or(0);
            value /= 10;
        }
        write_all(&output[index..]);
    }

    fn write_octal(mut value: u16) {
        let mut output = [b'0'; 4];
        for byte in output.iter_mut().rev() {
            *byte = b'0' + u8::try_from(value & 7).unwrap_or(0);
            value >>= 3;
        }
        write_all(&output);
    }

    fn write_padded_milliseconds(value: u64) {
        let hundreds = u8::try_from((value / 100) % 10).unwrap_or(0);
        let tens = u8::try_from((value / 10) % 10).unwrap_or(0);
        let ones = u8::try_from(value % 10).unwrap_or(0);
        write_all(&[b'0' + hundreds, b'0' + tens, b'0' + ones]);
    }

    fn trim(mut bytes: &[u8]) -> &[u8] {
        while bytes.first() == Some(&b' ') {
            bytes = &bytes[1..];
        }
        while bytes.last() == Some(&b' ') {
            bytes = &bytes[..bytes.len() - 1];
        }
        bytes
    }

    fn split_once(line: &[u8]) -> (&[u8], &[u8]) {
        let split = line
            .iter()
            .position(|byte| *byte == b' ')
            .unwrap_or(line.len());
        let arguments = line.get(split..).map_or(&[][..], trim);
        (&line[..split], arguments)
    }

    #[panic_handler]
    fn panic(_info: &PanicInfo<'_>) -> ! {
        write_all(b"nexsh: fatal userspace panic\n");
        runtime::exit(127)
    }
}

#[cfg(not(target_os = "none"))]
fn main() {}
