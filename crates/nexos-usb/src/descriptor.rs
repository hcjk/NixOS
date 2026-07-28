use crate::UsbError;

pub const TYPE_DEVICE: u8 = 1;
pub const TYPE_CONFIGURATION: u8 = 2;
pub const TYPE_STRING: u8 = 3;
pub const TYPE_INTERFACE: u8 = 4;
pub const TYPE_ENDPOINT: u8 = 5;
pub const TYPE_HID: u8 = 0x21;
pub const TYPE_HUB: u8 = 0x29;
pub const TYPE_SUPERSPEED_HUB: u8 = 0x2a;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Descriptor<'a> {
    pub descriptor_type: u8,
    pub bytes: &'a [u8],
}

pub struct DescriptorIter<'a> {
    remaining: &'a [u8],
    failed: bool,
}

impl<'a> DescriptorIter<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            failed: false,
        }
    }

    #[must_use]
    pub const fn failed(&self) -> bool {
        self.failed
    }
}

impl<'a> Iterator for DescriptorIter<'a> {
    type Item = Descriptor<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() || self.failed {
            return None;
        }
        if self.remaining.len() < 2 {
            self.failed = true;
            return None;
        }
        let length = usize::from(self.remaining[0]);
        if length < 2 || length > self.remaining.len() {
            self.failed = true;
            return None;
        }
        let (descriptor, rest) = self.remaining.split_at(length);
        self.remaining = rest;
        Some(Descriptor {
            descriptor_type: descriptor[1],
            bytes: descriptor,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub endpoint_zero_max_packet: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
    pub manufacturer_string: u8,
    pub product_string: u8,
    pub serial_string: u8,
    pub configuration_count: u8,
}

impl DeviceDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 18 || bytes[0] != 18 || bytes[1] != TYPE_DEVICE {
            return Err(UsbError::InvalidDescriptor);
        }
        let usb_version = le16(bytes, 2)?;
        let raw_packet = bytes[7];
        let endpoint_zero_max_packet = if usb_version >= 0x0300 {
            1_u16
                .checked_shl(u32::from(raw_packet))
                .ok_or(UsbError::InvalidDescriptor)?
        } else {
            u16::from(raw_packet)
        };
        if endpoint_zero_max_packet == 0 {
            return Err(UsbError::InvalidDescriptor);
        }
        Ok(Self {
            usb_version,
            device_class: bytes[4],
            device_subclass: bytes[5],
            device_protocol: bytes[6],
            endpoint_zero_max_packet,
            vendor_id: le16(bytes, 8)?,
            product_id: le16(bytes, 10)?,
            device_version: le16(bytes, 12)?,
            manufacturer_string: bytes[14],
            product_string: bytes[15],
            serial_string: bytes[16],
            configuration_count: bytes[17],
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationDescriptor {
    pub total_length: u16,
    pub interface_count: u8,
    pub configuration_value: u8,
    pub attributes: u8,
    pub max_power_milliamps: u16,
}

impl ConfigurationDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 9 || bytes[0] != 9 || bytes[1] != TYPE_CONFIGURATION {
            return Err(UsbError::InvalidDescriptor);
        }
        let total_length = le16(bytes, 2)?;
        if total_length < 9 {
            return Err(UsbError::InvalidDescriptor);
        }
        Ok(Self {
            total_length,
            interface_count: bytes[4],
            configuration_value: bytes[5],
            attributes: bytes[7],
            max_power_milliamps: u16::from(bytes[8]) * 2,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceDescriptor {
    pub number: u8,
    pub alternate_setting: u8,
    pub endpoint_count: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

impl InterfaceDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 9 || bytes[0] != 9 || bytes[1] != TYPE_INTERFACE {
            return Err(UsbError::InvalidDescriptor);
        }
        Ok(Self {
            number: bytes[2],
            alternate_setting: bytes[3],
            endpoint_count: bytes[4],
            class: bytes[5],
            subclass: bytes[6],
            protocol: bytes[7],
        })
    }

    #[must_use]
    pub const fn is_hid_boot_keyboard(self) -> bool {
        self.class == 3 && self.subclass == 1 && self.protocol == 1
    }

    #[must_use]
    pub const fn is_hid_boot_mouse(self) -> bool {
        self.class == 3 && self.subclass == 1 && self.protocol == 2
    }

    #[must_use]
    pub const fn is_mass_storage_bot(self) -> bool {
        self.class == 8 && self.protocol == 0x50
    }

    #[must_use]
    pub const fn is_hub(self) -> bool {
        self.class == 9
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Out,
    In,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointDescriptor {
    pub number: u8,
    pub direction: Direction,
    pub transfer_type: TransferType,
    pub max_packet_size: u16,
    pub interval: u8,
}

impl EndpointDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 7 || bytes[0] != 7 || bytes[1] != TYPE_ENDPOINT {
            return Err(UsbError::InvalidDescriptor);
        }
        let transfer_type = match bytes[3] & 3 {
            0 => TransferType::Control,
            1 => TransferType::Isochronous,
            2 => TransferType::Bulk,
            3 => TransferType::Interrupt,
            _ => unreachable!(),
        };
        Ok(Self {
            number: bytes[2] & 0x0f,
            direction: if bytes[2] & 0x80 == 0 {
                Direction::Out
            } else {
                Direction::In
            },
            transfer_type,
            max_packet_size: le16(bytes, 4)? & 0x07ff,
            interval: bytes[6],
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigurationSummary {
    pub descriptors: u16,
    pub interfaces: u8,
    pub endpoints: u8,
    pub hid_keyboards: u8,
    pub hid_mice: u8,
    pub hubs: u8,
    pub mass_storage: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassEndpoint {
    pub interface_number: u8,
    pub descriptor: EndpointDescriptor,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MassStorageEndpoints {
    pub interface_number: u8,
    pub bulk_in: Option<EndpointDescriptor>,
    pub bulk_out: Option<EndpointDescriptor>,
}

impl MassStorageEndpoints {
    #[must_use]
    pub const fn ready(self) -> bool {
        self.bulk_in.is_some() && self.bulk_out.is_some()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClassBindings {
    pub keyboard: Option<ClassEndpoint>,
    pub mouse: Option<ClassEndpoint>,
    pub hub: Option<ClassEndpoint>,
    pub mass_storage: Option<MassStorageEndpoints>,
}

pub fn classify_configuration(bytes: &[u8]) -> Result<ClassBindings, UsbError> {
    let config = ConfigurationDescriptor::parse(bytes)?;
    let total_length = usize::from(config.total_length);
    if bytes.len() < total_length {
        return Err(UsbError::BufferTooShort);
    }

    let mut bindings = ClassBindings::default();
    let mut interface = None;
    let mut descriptors = DescriptorIter::new(&bytes[..total_length]);
    for descriptor in descriptors.by_ref() {
        match descriptor.descriptor_type {
            TYPE_INTERFACE => {
                let parsed = InterfaceDescriptor::parse(descriptor.bytes)?;
                interface = (parsed.alternate_setting == 0).then_some(parsed);
                if parsed.is_mass_storage_bot() && bindings.mass_storage.is_none() {
                    bindings.mass_storage = Some(MassStorageEndpoints {
                        interface_number: parsed.number,
                        bulk_in: None,
                        bulk_out: None,
                    });
                }
            }
            TYPE_ENDPOINT => {
                let endpoint = EndpointDescriptor::parse(descriptor.bytes)?;
                let Some(interface) = interface else {
                    continue;
                };
                if endpoint.transfer_type == TransferType::Interrupt
                    && endpoint.direction == Direction::In
                {
                    let binding = ClassEndpoint {
                        interface_number: interface.number,
                        descriptor: endpoint,
                    };
                    if interface.is_hid_boot_keyboard() && bindings.keyboard.is_none() {
                        bindings.keyboard = Some(binding);
                    } else if interface.is_hid_boot_mouse() && bindings.mouse.is_none() {
                        bindings.mouse = Some(binding);
                    } else if interface.is_hub() && bindings.hub.is_none() {
                        bindings.hub = Some(binding);
                    }
                } else if interface.is_mass_storage_bot()
                    && endpoint.transfer_type == TransferType::Bulk
                    && let Some(mass_storage) = bindings
                        .mass_storage
                        .as_mut()
                        .filter(|binding| binding.interface_number == interface.number)
                {
                    match endpoint.direction {
                        Direction::In if mass_storage.bulk_in.is_none() => {
                            mass_storage.bulk_in = Some(endpoint);
                        }
                        Direction::Out if mass_storage.bulk_out.is_none() => {
                            mass_storage.bulk_out = Some(endpoint);
                        }
                        Direction::In | Direction::Out => {}
                    }
                }
            }
            _ => {}
        }
    }
    if descriptors.failed() {
        return Err(UsbError::InvalidDescriptor);
    }
    if bindings
        .mass_storage
        .is_some_and(|binding| !binding.ready())
    {
        bindings.mass_storage = None;
    }
    Ok(bindings)
}

pub fn summarize_configuration(bytes: &[u8]) -> Result<ConfigurationSummary, UsbError> {
    let config = ConfigurationDescriptor::parse(bytes)?;
    let total_length = usize::from(config.total_length);
    if bytes.len() < total_length {
        return Err(UsbError::BufferTooShort);
    }
    let mut summary = ConfigurationSummary::default();
    let mut descriptors = DescriptorIter::new(&bytes[..total_length]);
    for descriptor in descriptors.by_ref() {
        summary.descriptors = summary.descriptors.saturating_add(1);
        if descriptor.descriptor_type == TYPE_INTERFACE {
            let interface = InterfaceDescriptor::parse(descriptor.bytes)?;
            summary.interfaces = summary.interfaces.saturating_add(1);
            summary.hid_keyboards = summary
                .hid_keyboards
                .saturating_add(u8::from(interface.is_hid_boot_keyboard()));
            summary.hid_mice = summary
                .hid_mice
                .saturating_add(u8::from(interface.is_hid_boot_mouse()));
            summary.hubs = summary.hubs.saturating_add(u8::from(interface.is_hub()));
            summary.mass_storage = summary
                .mass_storage
                .saturating_add(u8::from(interface.is_mass_storage_bot()));
        } else if descriptor.descriptor_type == TYPE_ENDPOINT {
            EndpointDescriptor::parse(descriptor.bytes)?;
            summary.endpoints = summary.endpoints.saturating_add(1);
        }
    }
    if descriptors.failed() {
        return Err(UsbError::InvalidDescriptor);
    }
    Ok(summary)
}

fn le16(bytes: &[u8], offset: usize) -> Result<u16, UsbError> {
    let pair = bytes
        .get(offset..offset + 2)
        .ok_or(UsbError::BufferTooShort)?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb3_device_descriptor_packet_exponent() {
        let descriptor = [
            18, 1, 0x10, 0x03, 0, 0, 0, 9, 0x34, 0x12, 0x78, 0x56, 0, 1, 1, 2, 3, 1,
        ];
        let parsed = DeviceDescriptor::parse(&descriptor).unwrap();
        assert_eq!(parsed.usb_version, 0x0310);
        assert_eq!(parsed.endpoint_zero_max_packet, 512);
        assert_eq!(parsed.vendor_id, 0x1234);
        assert_eq!(parsed.product_id, 0x5678);
    }

    #[test]
    fn parses_usb2_device_descriptor_packet_bytes() {
        let descriptor = [
            18, 1, 0x00, 0x02, 0, 0, 0, 8, 0x27, 0x06, 0x01, 0x00, 0, 1, 1, 2, 0, 1,
        ];
        let parsed = DeviceDescriptor::parse(&descriptor).unwrap();
        assert_eq!(parsed.usb_version, 0x0200);
        assert_eq!(parsed.endpoint_zero_max_packet, 8);
    }

    #[test]
    fn iterates_and_classifies_configuration() {
        let bytes = [
            9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x01, 2, 0, 2, 0,
            7, 5, 0x82, 2, 0, 2, 0,
        ];
        let summary = summarize_configuration(&bytes).unwrap();
        assert_eq!(summary.descriptors, 4);
        assert_eq!(summary.interfaces, 1);
        assert_eq!(summary.endpoints, 2);
        assert_eq!(summary.mass_storage, 1);
    }

    #[test]
    fn rejects_zero_length_descriptor() {
        let bytes = [9, 2, 11, 0, 0, 1, 0, 0x80, 0, 0, 4];
        assert_eq!(
            summarize_configuration(&bytes),
            Err(UsbError::InvalidDescriptor)
        );
    }

    #[test]
    fn parses_interrupt_input_endpoint() {
        let endpoint = EndpointDescriptor::parse(&[7, 5, 0x83, 3, 64, 0, 8]).unwrap();
        assert_eq!(endpoint.number, 3);
        assert_eq!(endpoint.direction, Direction::In);
        assert_eq!(endpoint.transfer_type, TransferType::Interrupt);
        assert_eq!(endpoint.max_packet_size, 64);
    }

    #[test]
    fn binds_boot_hid_and_mass_storage_endpoints() {
        let bytes = [
            9, 2, 73, 0, 3, 1, 0, 0x80, 50, // configuration
            9, 4, 0, 0, 1, 3, 1, 1, 0, // boot keyboard
            9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID
            7, 5, 0x81, 3, 8, 0, 10, // interrupt IN
            9, 4, 1, 0, 1, 3, 1, 2, 0, // boot mouse
            7, 5, 0x82, 3, 4, 0, 8, // interrupt IN
            9, 4, 2, 0, 2, 8, 6, 0x50, 0, // BOT/SCSI
            7, 5, 0x03, 2, 0, 2, 0, // bulk OUT
            7, 5, 0x84, 2, 0, 2, 0, // bulk IN
        ];
        let bindings = classify_configuration(&bytes).unwrap();
        assert_eq!(bindings.keyboard.unwrap().descriptor.number, 1);
        assert_eq!(bindings.mouse.unwrap().descriptor.number, 2);
        let mass_storage = bindings.mass_storage.unwrap();
        assert!(mass_storage.ready());
        assert_eq!(mass_storage.interface_number, 2);
        assert_eq!(mass_storage.bulk_out.unwrap().number, 3);
        assert_eq!(mass_storage.bulk_in.unwrap().number, 4);
    }
}
