use crate::UsbError;

pub const CBW_SIGNATURE: u32 = 0x4342_5355;
pub const CSW_SIGNATURE: u32 = 0x5342_5355;
pub const CBW_BYTES: usize = 31;
pub const CSW_BYTES: usize = 13;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataDirection {
    None,
    Out,
    In,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScsiCommand {
    bytes: [u8; 16],
    length: u8,
    transfer_bytes: u32,
    direction: DataDirection,
}

impl ScsiCommand {
    #[must_use]
    pub const fn inquiry(allocation_length: u8) -> Self {
        let mut bytes = [0_u8; 16];
        bytes[0] = 0x12;
        bytes[4] = allocation_length;
        Self {
            bytes,
            length: 6,
            transfer_bytes: allocation_length as u32,
            direction: DataDirection::In,
        }
    }

    #[must_use]
    pub const fn test_unit_ready() -> Self {
        let mut bytes = [0_u8; 16];
        bytes[0] = 0x00;
        Self {
            bytes,
            length: 6,
            transfer_bytes: 0,
            direction: DataDirection::None,
        }
    }

    #[must_use]
    pub const fn request_sense(allocation_length: u8) -> Self {
        let mut bytes = [0_u8; 16];
        bytes[0] = 0x03;
        bytes[4] = allocation_length;
        Self {
            bytes,
            length: 6,
            transfer_bytes: allocation_length as u32,
            direction: DataDirection::In,
        }
    }

    #[must_use]
    pub const fn read_capacity_10() -> Self {
        let mut bytes = [0_u8; 16];
        bytes[0] = 0x25;
        Self {
            bytes,
            length: 10,
            transfer_bytes: 8,
            direction: DataDirection::In,
        }
    }

    #[must_use]
    pub fn read_10(lba: u32, blocks: u16, block_size: u32) -> Option<Self> {
        Self::read_write_10(0x28, lba, blocks, block_size, DataDirection::In)
    }

    #[must_use]
    pub fn write_10(lba: u32, blocks: u16, block_size: u32) -> Option<Self> {
        Self::read_write_10(0x2a, lba, blocks, block_size, DataDirection::Out)
    }

    #[must_use]
    pub const fn synchronize_cache_10() -> Self {
        let mut bytes = [0_u8; 16];
        bytes[0] = 0x35;
        Self {
            bytes,
            length: 10,
            transfer_bytes: 0,
            direction: DataDirection::None,
        }
    }

    fn read_write_10(
        operation: u8,
        lba: u32,
        blocks: u16,
        block_size: u32,
        direction: DataDirection,
    ) -> Option<Self> {
        let transfer_bytes = u32::from(blocks).checked_mul(block_size)?;
        let mut bytes = [0_u8; 16];
        bytes[0] = operation;
        bytes[2..6].copy_from_slice(&lba.to_be_bytes());
        bytes[7..9].copy_from_slice(&blocks.to_be_bytes());
        Some(Self {
            bytes,
            length: 10,
            transfer_bytes,
            direction,
        })
    }

    #[must_use]
    pub fn bytes(self) -> [u8; 16] {
        self.bytes
    }

    #[must_use]
    pub const fn length(self) -> u8 {
        self.length
    }

    #[must_use]
    pub const fn transfer_bytes(self) -> u32 {
        self.transfer_bytes
    }

    #[must_use]
    pub const fn direction(self) -> DataDirection {
        self.direction
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandBlockWrapper {
    pub tag: u32,
    pub transfer_bytes: u32,
    pub direction: DataDirection,
    pub logical_unit: u8,
    pub command: ScsiCommand,
}

impl CommandBlockWrapper {
    pub fn encode(self, output: &mut [u8]) -> Result<(), UsbError> {
        if output.len() < CBW_BYTES || self.logical_unit > 15 {
            return Err(UsbError::BufferTooShort);
        }
        output[..CBW_BYTES].fill(0);
        output[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        output[4..8].copy_from_slice(&self.tag.to_le_bytes());
        output[8..12].copy_from_slice(&self.transfer_bytes.to_le_bytes());
        output[12] = match self.direction {
            DataDirection::In => 0x80,
            DataDirection::None | DataDirection::Out => 0,
        };
        output[13] = self.logical_unit;
        output[14] = self.command.length;
        let length = usize::from(self.command.length);
        output[15..15 + length].copy_from_slice(&self.command.bytes[..length]);
        Ok(())
    }

    #[must_use]
    pub const fn from_scsi(tag: u32, logical_unit: u8, command: ScsiCommand) -> Self {
        Self {
            tag,
            transfer_bytes: command.transfer_bytes,
            direction: command.direction,
            logical_unit,
            command,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Passed,
    Failed,
    PhaseError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandStatusWrapper {
    pub tag: u32,
    pub residue: u32,
    pub status: CommandStatus,
}

impl CommandStatusWrapper {
    pub fn parse(bytes: &[u8], expected_tag: u32) -> Result<Self, UsbError> {
        if bytes.len() < CSW_BYTES {
            return Err(UsbError::BufferTooShort);
        }
        let signature = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if signature != CSW_SIGNATURE {
            return Err(UsbError::InvalidSignature);
        }
        let tag = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if tag != expected_tag {
            return Err(UsbError::InvalidTag);
        }
        let status = match bytes[12] {
            0 => CommandStatus::Passed,
            1 => CommandStatus::Failed,
            2 => CommandStatus::PhaseError,
            _ => return Err(UsbError::InvalidStatus),
        };
        Ok(Self {
            tag,
            residue: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            status,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity10 {
    pub last_lba: u32,
    pub block_size: u32,
}

impl Capacity10 {
    pub fn parse(bytes: &[u8]) -> Result<Self, UsbError> {
        if bytes.len() < 8 {
            return Err(UsbError::BufferTooShort);
        }
        let last_lba = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let block_size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if block_size == 0 {
            return Err(UsbError::InvalidPacket);
        }
        Ok(Self {
            last_lba,
            block_size,
        })
    }

    #[must_use]
    pub const fn block_count(self) -> u64 {
        self.last_lba as u64 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_read_10_cbw() {
        let command = ScsiCommand::read_10(0x1020_3040, 8, 512).unwrap();
        let wrapper = CommandBlockWrapper::from_scsi(0x1122_3344, 0, command);
        let mut bytes = [0xcc; CBW_BYTES];
        wrapper.encode(&mut bytes).unwrap();
        assert_eq!(&bytes[0..4], &CBW_SIGNATURE.to_le_bytes());
        assert_eq!(&bytes[4..8], &0x1122_3344_u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &4096_u32.to_le_bytes());
        assert_eq!(bytes[12], 0x80);
        assert_eq!(bytes[14], 10);
        assert_eq!(bytes[15], 0x28);
        assert_eq!(&bytes[17..21], &0x1020_3040_u32.to_be_bytes());
    }

    #[test]
    fn validates_csw_signature_tag_and_status() {
        let mut bytes = [0_u8; CSW_BYTES];
        bytes[0..4].copy_from_slice(&CSW_SIGNATURE.to_le_bytes());
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&512_u32.to_le_bytes());
        bytes[12] = 1;
        let status = CommandStatusWrapper::parse(&bytes, 7).unwrap();
        assert_eq!(status.status, CommandStatus::Failed);
        assert_eq!(status.residue, 512);
        assert_eq!(
            CommandStatusWrapper::parse(&bytes, 8),
            Err(UsbError::InvalidTag)
        );
    }

    #[test]
    fn parses_capacity_as_big_endian() {
        let capacity = Capacity10::parse(&[0, 0, 0x1f, 0xff, 0, 0, 2, 0]).unwrap();
        assert_eq!(capacity.block_count(), 8192);
        assert_eq!(capacity.block_size, 512);
    }
}
