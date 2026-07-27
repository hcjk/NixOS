#![no_std]
#![allow(clippy::missing_errors_doc)]

#[cfg(test)]
extern crate std;

pub mod descriptor;
pub mod enumeration;
pub mod hid;
pub mod hub;
pub mod mass_storage;
pub mod xhci;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbError {
    BufferTooShort,
    InvalidDescriptor,
    InvalidPacket,
    InvalidSignature,
    InvalidTag,
    InvalidStatus,
    RingFull,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UsbSpeed {
    Low = 1,
    Full = 2,
    High = 3,
    Super = 4,
    SuperPlus = 5,
    Unknown = 0xff,
}

impl UsbSpeed {
    #[must_use]
    pub const fn from_xhci_port_speed(value: u8) -> Self {
        match value {
            1 => Self::Full,
            2 => Self::Low,
            3 => Self::High,
            4 => Self::Super,
            5 => Self::SuperPlus,
            _ => Self::Unknown,
        }
    }
}
