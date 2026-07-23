use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

const QUEUE_CAPACITY: usize = 256;

struct ScancodeQueue {
    bytes: UnsafeCell<[u8; QUEUE_CAPACITY]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: The IRQ handler is the only producer and the monitor is the only
// consumer. Atomic indices publish access to individual queue slots.
unsafe impl Sync for ScancodeQueue {}

static SCANCODES: ScancodeQueue = ScancodeQueue {
    bytes: UnsafeCell::new([0; QUEUE_CAPACITY]),
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
};

pub fn enable_keyboard_interrupt() {
    // SAFETY: NexOS owns the i8042 controller. The bounded waits prevent a
    // missing controller from hanging boot indefinitely.
    unsafe {
        if !wait_input_clear() {
            return;
        }
        outb(0x64, 0xae);
        if !wait_input_clear() {
            return;
        }
        outb(0x64, 0x20);
        if !wait_output_full() {
            return;
        }
        let command_byte = inb(0x60);
        if !wait_input_clear() {
            return;
        }
        outb(0x64, 0x60);
        if !wait_input_clear() {
            return;
        }
        // Enable first-port IRQ delivery and its clock while preserving the
        // firmware-selected translation mode.
        outb(0x60, (command_byte | 1) & !0x10);
    }
}

pub fn enqueue_scancode(scancode: u8) {
    let tail = SCANCODES.tail.load(Ordering::Relaxed);
    let next = (tail + 1) % QUEUE_CAPACITY;
    if next == SCANCODES.head.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: Only the IRQ producer writes the slot at the unpublished tail.
    unsafe { (*SCANCODES.bytes.get())[tail] = scancode };
    SCANCODES.tail.store(next, Ordering::Release);
}

fn dequeue_scancode() -> Option<u8> {
    let head = SCANCODES.head.load(Ordering::Relaxed);
    if head == SCANCODES.tail.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: The acquire load observed the producer's publication of this
    // slot, and only this consumer advances the head.
    let scancode = unsafe { (*SCANCODES.bytes.get())[head] };
    SCANCODES
        .head
        .store((head + 1) % QUEUE_CAPACITY, Ordering::Release);
    Some(scancode)
}

pub struct Keyboard {
    shift: bool,
    caps_lock: bool,
    extended: bool,
}

impl Keyboard {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            shift: false,
            caps_lock: false,
            extended: false,
        }
    }

    pub fn read_character(&mut self) -> Option<u8> {
        self.translate(dequeue_scancode()?)
    }

    fn translate(&mut self, scancode: u8) -> Option<u8> {
        if scancode == 0xe0 {
            self.extended = true;
            return None;
        }
        if self.extended {
            self.extended = false;
            return None;
        }
        let released = scancode & 0x80 != 0;
        let code = scancode & 0x7f;
        match code {
            0x2a | 0x36 => {
                self.shift = !released;
                return None;
            }
            0x3a if !released => {
                self.caps_lock = !self.caps_lock;
                return None;
            }
            _ if released => return None,
            _ => {}
        }
        match code {
            0x0e => Some(8),
            0x1c => Some(b'\n'),
            0x39 => Some(b' '),
            _ => {
                let base = map_scancode(code)?;
                Some(apply_shift(base, self.shift, self.caps_lock))
            }
        }
    }
}

pub fn reboot() -> ! {
    for _ in 0..100_000 {
        // SAFETY: Reading i8042 status at CPL0 is valid.
        if unsafe { inb(0x64) } & 2 == 0 {
            // SAFETY: 0xFE requests a CPU reset through the i8042.
            unsafe { outb(0x64, 0xfe) };
            break;
        }
        core::hint::spin_loop();
    }
    loop {
        // SAFETY: HLT is valid in kernel mode.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

fn map_scancode(code: u8) -> Option<u8> {
    Some(match code {
        0x02 => b'1',
        0x03 => b'2',
        0x04 => b'3',
        0x05 => b'4',
        0x06 => b'5',
        0x07 => b'6',
        0x08 => b'7',
        0x09 => b'8',
        0x0a => b'9',
        0x0b => b'0',
        0x0c => b'-',
        0x0d => b'=',
        0x10 => b'q',
        0x11 => b'w',
        0x12 => b'e',
        0x13 => b'r',
        0x14 => b't',
        0x15 => b'y',
        0x16 => b'u',
        0x17 => b'i',
        0x18 => b'o',
        0x19 => b'p',
        0x1a => b'[',
        0x1b => b']',
        0x1e => b'a',
        0x1f => b's',
        0x20 => b'd',
        0x21 => b'f',
        0x22 => b'g',
        0x23 => b'h',
        0x24 => b'j',
        0x25 => b'k',
        0x26 => b'l',
        0x27 => b';',
        0x28 => b'\'',
        0x29 => b'`',
        0x2b => b'\\',
        0x2c => b'z',
        0x2d => b'x',
        0x2e => b'c',
        0x2f => b'v',
        0x30 => b'b',
        0x31 => b'n',
        0x32 => b'm',
        0x33 => b',',
        0x34 => b'.',
        0x35 => b'/',
        _ => return None,
    })
}

fn apply_shift(character: u8, shift: bool, caps_lock: bool) -> u8 {
    if character.is_ascii_lowercase() {
        return if shift ^ caps_lock {
            character.to_ascii_uppercase()
        } else {
            character
        };
    }
    if !shift {
        return character;
    }
    match character {
        b'1' => b'!',
        b'2' => b'@',
        b'3' => b'#',
        b'4' => b'$',
        b'5' => b'%',
        b'6' => b'^',
        b'7' => b'&',
        b'8' => b'*',
        b'9' => b'(',
        b'0' => b')',
        b'-' => b'_',
        b'=' => b'+',
        b'[' => b'{',
        b']' => b'}',
        b';' => b':',
        b'\'' => b'"',
        b'`' => b'~',
        b'\\' => b'|',
        b',' => b'<',
        b'.' => b'>',
        b'/' => b'?',
        _ => character,
    }
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

unsafe fn wait_input_clear() -> bool {
    for _ in 0..100_000 {
        // SAFETY: The caller owns the i8042 status port.
        if unsafe { inb(0x64) } & 2 == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

unsafe fn wait_output_full() -> bool {
    for _ in 0..100_000 {
        // SAFETY: The caller owns the i8042 status port.
        if unsafe { inb(0x64) } & 1 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}
