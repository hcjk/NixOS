use crate::UsbError;

pub const KEYBOARD_REPORT_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyboardReport {
    pub modifiers: u8,
    pub keys: [u8; 6],
}

impl KeyboardReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < KEYBOARD_REPORT_BYTES {
            return Err(UsbError::BufferTooShort);
        }
        let mut keys = [0_u8; 6];
        keys.copy_from_slice(&bytes[2..8]);
        if keys.iter().any(|key| (1..=3).contains(key)) {
            return Err(UsbError::InvalidPacket);
        }
        Ok(Self {
            modifiers: bytes[0],
            keys,
        })
    }

    #[must_use]
    pub fn key_pressed_since(self, previous: Self, key: u8) -> bool {
        key != 0 && self.keys.contains(&key) && !previous.keys.contains(&key)
    }

    #[must_use]
    pub const fn left_shift(self) -> bool {
        self.modifiers & (1 << 1) != 0
    }

    #[must_use]
    pub const fn right_shift(self) -> bool {
        self.modifiers & (1 << 5) != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MouseReport {
    pub buttons: u8,
    pub delta_x: i8,
    pub delta_y: i8,
    pub wheel: i8,
}

impl MouseReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 3 {
            return Err(UsbError::BufferTooShort);
        }
        Ok(Self {
            buttons: bytes[0] & 0x1f,
            delta_x: bytes[1].cast_signed(),
            delta_y: bytes[2].cast_signed(),
            wheel: bytes.get(3).copied().unwrap_or(0).cast_signed(),
        })
    }
}

#[must_use]
pub const fn boot_key_ascii(usage: u8, shifted: bool) -> Option<u8> {
    match usage {
        0x04..=0x1d => {
            let letter = b'a' + (usage - 0x04);
            Some(if shifted {
                letter.to_ascii_uppercase()
            } else {
                letter
            })
        }
        0x1e..=0x26 => {
            const NORMAL: &[u8; 9] = b"123456789";
            const SHIFTED: &[u8; 9] = b"!@#$%^&*(";
            let index = (usage - 0x1e) as usize;
            Some(if shifted {
                SHIFTED[index]
            } else {
                NORMAL[index]
            })
        }
        0x27 => Some(if shifted { b')' } else { b'0' }),
        0x28 => Some(b'\n'),
        0x2a => Some(8),
        0x2b => Some(b'\t'),
        0x2c => Some(b' '),
        0x2d => Some(if shifted { b'_' } else { b'-' }),
        0x2e => Some(if shifted { b'+' } else { b'=' }),
        0x2f => Some(if shifted { b'{' } else { b'[' }),
        0x30 => Some(if shifted { b'}' } else { b']' }),
        0x31 => Some(if shifted { b'|' } else { b'\\' }),
        0x33 => Some(if shifted { b':' } else { b';' }),
        0x34 => Some(if shifted { b'"' } else { b'\'' }),
        0x35 => Some(if shifted { b'~' } else { b'`' }),
        0x36 => Some(if shifted { b'<' } else { b',' }),
        0x37 => Some(if shifted { b'>' } else { b'.' }),
        0x38 => Some(if shifted { b'?' } else { b'/' }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_new_keyboard_press() {
        let old = KeyboardReport::parse(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap();
        let new = KeyboardReport::parse(&[2, 0, 4, 5, 0, 0, 0, 0]).unwrap();
        assert!(!new.key_pressed_since(old, 4));
        assert!(new.key_pressed_since(old, 5));
        assert!(new.left_shift());
        assert_eq!(boot_key_ascii(5, true), Some(b'B'));
    }

    #[test]
    fn rejects_keyboard_rollover_report() {
        assert_eq!(
            KeyboardReport::parse(&[0, 0, 1, 1, 1, 1, 1, 1]),
            Err(UsbError::InvalidPacket)
        );
    }

    #[test]
    fn parses_mouse_motion_and_wheel() {
        let report = MouseReport::parse(&[5, 0xfe, 4, 0xff]).unwrap();
        assert_eq!(report.buttons, 5);
        assert_eq!(report.delta_x, -2);
        assert_eq!(report.delta_y, 4);
        assert_eq!(report.wheel, -1);
    }
}
