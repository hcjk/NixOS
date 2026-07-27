use crate::{UsbError, UsbSpeed};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumerationState {
    Detached,
    Debouncing,
    Resetting,
    ReadingDescriptor8,
    Addressing,
    ReadingDeviceDescriptor,
    ReadingConfiguration,
    Configuring,
    Ready,
    Failed(UsbError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumerationAction {
    WaitForStableConnection,
    ResetPort,
    ReadDescriptor8,
    SetAddress,
    ReadDeviceDescriptor,
    ReadConfiguration,
    SetConfiguration,
    BindDrivers,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Enumerator {
    state: EnumerationState,
    speed: UsbSpeed,
    address: u8,
}

impl Enumerator {
    #[must_use]
    pub const fn new(speed: UsbSpeed) -> Self {
        Self {
            state: EnumerationState::Detached,
            speed,
            address: 0,
        }
    }

    #[must_use]
    pub const fn state(self) -> EnumerationState {
        self.state
    }

    #[must_use]
    pub const fn speed(self) -> UsbSpeed {
        self.speed
    }

    #[must_use]
    pub const fn address(self) -> u8 {
        self.address
    }

    pub const fn connected(&mut self) -> EnumerationAction {
        self.state = EnumerationState::Debouncing;
        EnumerationAction::WaitForStableConnection
    }

    pub const fn stable(&mut self) -> EnumerationAction {
        self.state = EnumerationState::Resetting;
        EnumerationAction::ResetPort
    }

    pub const fn reset_complete(&mut self) -> EnumerationAction {
        self.state = EnumerationState::ReadingDescriptor8;
        EnumerationAction::ReadDescriptor8
    }

    pub const fn descriptor8_complete(&mut self) -> EnumerationAction {
        self.state = EnumerationState::Addressing;
        EnumerationAction::SetAddress
    }

    pub fn address_complete(&mut self, address: u8) -> Result<EnumerationAction, UsbError> {
        if address == 0 || address > 127 {
            self.fail(UsbError::InvalidPacket);
            return Err(UsbError::InvalidPacket);
        }
        self.address = address;
        self.state = EnumerationState::ReadingDeviceDescriptor;
        Ok(EnumerationAction::ReadDeviceDescriptor)
    }

    pub const fn device_descriptor_complete(&mut self) -> EnumerationAction {
        self.state = EnumerationState::ReadingConfiguration;
        EnumerationAction::ReadConfiguration
    }

    pub const fn configuration_read(&mut self) -> EnumerationAction {
        self.state = EnumerationState::Configuring;
        EnumerationAction::SetConfiguration
    }

    pub const fn configuration_set(&mut self) -> EnumerationAction {
        self.state = EnumerationState::Ready;
        EnumerationAction::BindDrivers
    }

    pub const fn disconnect(&mut self) {
        self.state = EnumerationState::Detached;
        self.address = 0;
    }

    pub const fn fail(&mut self, error: UsbError) {
        self.state = EnumerationState::Failed(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_usb_enumeration_sequence() {
        let mut enumerator = Enumerator::new(UsbSpeed::High);
        assert_eq!(
            enumerator.connected(),
            EnumerationAction::WaitForStableConnection
        );
        assert_eq!(enumerator.stable(), EnumerationAction::ResetPort);
        assert_eq!(
            enumerator.reset_complete(),
            EnumerationAction::ReadDescriptor8
        );
        assert_eq!(
            enumerator.descriptor8_complete(),
            EnumerationAction::SetAddress
        );
        assert_eq!(
            enumerator.address_complete(7).unwrap(),
            EnumerationAction::ReadDeviceDescriptor
        );
        assert_eq!(
            enumerator.device_descriptor_complete(),
            EnumerationAction::ReadConfiguration
        );
        assert_eq!(
            enumerator.configuration_read(),
            EnumerationAction::SetConfiguration
        );
        assert_eq!(
            enumerator.configuration_set(),
            EnumerationAction::BindDrivers
        );
        assert_eq!(enumerator.state(), EnumerationState::Ready);
        assert_eq!(enumerator.address(), 7);
    }

    #[test]
    fn rejects_reserved_address_zero() {
        let mut enumerator = Enumerator::new(UsbSpeed::Full);
        assert_eq!(enumerator.address_complete(0), Err(UsbError::InvalidPacket));
        assert_eq!(
            enumerator.state(),
            EnumerationState::Failed(UsbError::InvalidPacket)
        );
    }
}
