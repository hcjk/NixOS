use crate::UsbError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HubDescriptor {
    pub ports: u8,
    pub characteristics: u16,
    pub power_on_delay_ms: u16,
    pub controller_current_ma: u8,
}

impl HubDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 7 || bytes[0] < 7 || !matches!(bytes[1], 0x29 | 0x2a) {
            return Err(UsbError::InvalidDescriptor);
        }
        Ok(Self {
            ports: bytes[2],
            characteristics: u16::from_le_bytes([bytes[3], bytes[4]]),
            power_on_delay_ms: u16::from(bytes[5]) * 2,
            controller_current_ma: bytes[6],
        })
    }

    #[must_use]
    pub const fn individually_powered(self) -> bool {
        self.characteristics & 3 == 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct HubPortStatus {
    pub connected: bool,
    pub enabled: bool,
    pub suspended: bool,
    pub over_current: bool,
    pub resetting: bool,
    pub powered: bool,
    pub low_speed: bool,
    pub high_speed: bool,
    pub connect_changed: bool,
    pub enable_changed: bool,
    pub reset_changed: bool,
}

impl HubPortStatus {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 4 {
            return Err(UsbError::BufferTooShort);
        }
        let status = u16::from_le_bytes([bytes[0], bytes[1]]);
        let change = u16::from_le_bytes([bytes[2], bytes[3]]);
        Ok(Self {
            connected: status & 1 != 0,
            enabled: status & (1 << 1) != 0,
            suspended: status & (1 << 2) != 0,
            over_current: status & (1 << 3) != 0,
            resetting: status & (1 << 4) != 0,
            powered: status & (1 << 8) != 0,
            low_speed: status & (1 << 9) != 0,
            high_speed: status & (1 << 10) != 0,
            connect_changed: change & 1 != 0,
            enable_changed: change & (1 << 1) != 0,
            reset_changed: change & (1 << 4) != 0,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HubPortIterator {
    next: u8,
    count: u8,
}

impl HubPortIterator {
    #[must_use]
    pub const fn new(port_count: u8) -> Self {
        Self {
            next: 1,
            count: port_count,
        }
    }
}

impl Iterator for HubPortIterator {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next > self.count {
            return None;
        }
        let port = self.next;
        self.next = self.next.saturating_add(1);
        Some(port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hub_and_port_status() {
        let descriptor = HubDescriptor::parse(&[9, 0x29, 4, 1, 0, 25, 10, 0, 0xff]).unwrap();
        assert_eq!(descriptor.ports, 4);
        assert_eq!(descriptor.power_on_delay_ms, 50);
        assert!(descriptor.individually_powered());

        let status = HubPortStatus::parse(&[3, 5, 0x11, 0]).unwrap();
        assert!(status.connected);
        assert!(status.enabled);
        assert!(status.powered);
        assert!(status.high_speed);
        assert!(status.connect_changed);
        assert!(status.reset_changed);
    }

    #[test]
    fn downstream_ports_are_one_based() {
        assert_eq!(
            HubPortIterator::new(3).collect::<std::vec::Vec<_>>(),
            [1, 2, 3]
        );
    }
}
