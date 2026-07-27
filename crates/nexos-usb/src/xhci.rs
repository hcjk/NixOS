use crate::UsbError;

pub const TRB_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C, align(16))]
pub struct Trb {
    pub parameter_low: u32,
    pub parameter_high: u32,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    #[must_use]
    pub const fn command(trb_type: TrbType) -> Self {
        Self {
            parameter_low: 0,
            parameter_high: 0,
            status: 0,
            control: (trb_type as u32) << 10,
        }
    }

    #[must_use]
    pub const fn with_parameter(mut self, parameter: u64) -> Self {
        let bytes = parameter.to_le_bytes();
        self.parameter_low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.parameter_high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        self
    }

    #[must_use]
    pub const fn with_status(mut self, status: u32) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub const fn with_control_bits(mut self, bits: u32) -> Self {
        self.control |= bits;
        self
    }

    #[must_use]
    pub const fn trb_type(self) -> Option<TrbType> {
        TrbType::from_raw(((self.control >> 10) & 0x3f) as u8)
    }

    #[must_use]
    pub const fn parameter(self) -> u64 {
        self.parameter_low as u64 | ((self.parameter_high as u64) << 32)
    }

    #[must_use]
    pub const fn cycle(self) -> bool {
        self.control & 1 != 0
    }

    #[must_use]
    pub const fn completion_code(self) -> u8 {
        (self.status >> 24) as u8
    }

    #[must_use]
    pub const fn slot_id(self) -> u8 {
        (self.control >> 24) as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TrbType {
    Normal = 1,
    SetupStage = 2,
    DataStage = 3,
    StatusStage = 4,
    Link = 6,
    EnableSlotCommand = 9,
    DisableSlotCommand = 10,
    AddressDeviceCommand = 11,
    ConfigureEndpointCommand = 12,
    EvaluateContextCommand = 13,
    ResetEndpointCommand = 14,
    StopEndpointCommand = 15,
    SetTransferRingDequeuePointerCommand = 16,
    ResetDeviceCommand = 17,
    NoOpCommand = 23,
    TransferEvent = 32,
    CommandCompletionEvent = 33,
    PortStatusChangeEvent = 34,
}

impl TrbType {
    #[must_use]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::Normal,
            2 => Self::SetupStage,
            3 => Self::DataStage,
            4 => Self::StatusStage,
            6 => Self::Link,
            9 => Self::EnableSlotCommand,
            10 => Self::DisableSlotCommand,
            11 => Self::AddressDeviceCommand,
            12 => Self::ConfigureEndpointCommand,
            13 => Self::EvaluateContextCommand,
            14 => Self::ResetEndpointCommand,
            15 => Self::StopEndpointCommand,
            16 => Self::SetTransferRingDequeuePointerCommand,
            17 => Self::ResetDeviceCommand,
            23 => Self::NoOpCommand,
            32 => Self::TransferEvent,
            33 => Self::CommandCompletionEvent,
            34 => Self::PortStatusChangeEvent,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingEntry {
    pub index: usize,
    pub trb: Trb,
}

pub struct ProducerRing<const N: usize> {
    entries: [Trb; N],
    enqueue: usize,
    cycle: bool,
}

impl<const N: usize> ProducerRing<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [Trb {
                parameter_low: 0,
                parameter_high: 0,
                status: 0,
                control: 0,
            }; N],
            enqueue: 0,
            cycle: true,
        }
    }

    pub fn push(&mut self, mut trb: Trb) -> Result<RingEntry, UsbError> {
        if N < 2 {
            return Err(UsbError::RingFull);
        }
        if self.enqueue == N - 1 {
            self.entries[self.enqueue] =
                Trb::command(TrbType::Link).with_control_bits(u32::from(self.cycle) | (1 << 1));
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        trb.control = (trb.control & !1) | u32::from(self.cycle);
        let index = self.enqueue;
        self.entries[index] = trb;
        self.enqueue += 1;
        Ok(RingEntry { index, trb })
    }

    #[must_use]
    pub const fn entries(&self) -> &[Trb; N] {
        &self.entries
    }

    #[must_use]
    pub const fn cycle(&self) -> bool {
        self.cycle
    }

    #[must_use]
    pub const fn enqueue_index(&self) -> usize {
        self.enqueue
    }
}

impl<const N: usize> Default for ProducerRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventConsumer {
    dequeue: usize,
    cycle: bool,
}

impl EventConsumer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dequeue: 0,
            cycle: true,
        }
    }

    #[must_use]
    pub fn consume<const N: usize>(&mut self, entries: &[Trb; N]) -> Option<RingEntry> {
        if N == 0 {
            return None;
        }
        let trb = entries[self.dequeue];
        if trb.cycle() != self.cycle {
            return None;
        }
        let index = self.dequeue;
        self.dequeue += 1;
        if self.dequeue == N {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(RingEntry { index, trb })
    }

    #[must_use]
    pub const fn dequeue_index(self) -> usize {
        self.dequeue
    }

    #[must_use]
    pub const fn cycle(self) -> bool {
        self.cycle
    }
}

impl Default for EventConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct EventRingSegmentTableEntry {
    pub ring_segment_base: u64,
    pub ring_segment_size: u32,
    pub reserved: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PortStatus {
    pub connected: bool,
    pub enabled: bool,
    pub powered: bool,
    pub resetting: bool,
    pub link_state: u8,
    pub speed_id: u8,
    pub connection_changed: bool,
}

impl PortStatus {
    #[must_use]
    pub const fn from_portsc(value: u32) -> Self {
        Self {
            connected: value & 1 != 0,
            enabled: value & (1 << 1) != 0,
            resetting: value & (1 << 4) != 0,
            link_state: ((value >> 5) & 0x0f) as u8,
            powered: value & (1 << 9) != 0,
            speed_id: ((value >> 10) & 0x0f) as u8,
            connection_changed: value & (1 << 17) != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_ring_inserts_link_and_toggles_cycle() {
        let mut ring = ProducerRing::<4>::new();
        assert_eq!(
            ring.push(Trb::command(TrbType::EnableSlotCommand))
                .unwrap()
                .index,
            0
        );
        ring.push(Trb::command(TrbType::NoOpCommand)).unwrap();
        ring.push(Trb::command(TrbType::NoOpCommand)).unwrap();
        let wrapped = ring.push(Trb::command(TrbType::NoOpCommand)).unwrap();
        assert_eq!(wrapped.index, 0);
        assert!(!wrapped.trb.cycle());
        assert_eq!(ring.entries()[3].trb_type(), Some(TrbType::Link));
        assert_ne!(ring.entries()[3].control & (1 << 1), 0);
    }

    #[test]
    fn event_consumer_tracks_hardware_cycle() {
        let mut events = [Trb::default(); 2];
        events[0] = Trb::command(TrbType::CommandCompletionEvent).with_control_bits(1);
        events[1] = Trb::command(TrbType::PortStatusChangeEvent).with_control_bits(1);
        let mut consumer = EventConsumer::new();
        assert_eq!(consumer.consume(&events).unwrap().index, 0);
        assert_eq!(consumer.consume(&events).unwrap().index, 1);
        assert!(consumer.consume(&events).is_none());
        events[0] = Trb::command(TrbType::TransferEvent);
        assert_eq!(consumer.consume(&events).unwrap().index, 0);
    }

    #[test]
    fn decodes_port_status_and_completion() {
        let status = PortStatus::from_portsc(1 | (1 << 1) | (1 << 9) | (4 << 10));
        assert!(status.connected);
        assert!(status.enabled);
        assert!(status.powered);
        assert_eq!(status.speed_id, 4);

        let event = Trb {
            status: 1 << 24,
            control: (33 << 10) | (6 << 24) | 1,
            ..Trb::default()
        };
        assert_eq!(event.completion_code(), 1);
        assert_eq!(event.slot_id(), 6);
    }
}
