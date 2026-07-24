use core::arch::asm;
use core::hint::spin_loop;

use nexos_storage::{BlockDevice, StorageError};

use crate::pci::{PciDevice, PciInventory};

const SECTOR_SIZE: usize = 512;
const STATUS_ERR: u8 = 1;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_DF: u8 = 1 << 5;
const STATUS_BSY: u8 = 1 << 7;
const POLL_LIMIT: usize = 2_000_000;

#[derive(Clone, Copy)]
pub struct IdeDevice {
    io_base: u16,
    control: u16,
    slave: bool,
    sector_count: u64,
    lba48: bool,
    model: [u8; 40],
    model_length: u8,
}

impl IdeDevice {
    #[must_use]
    pub const fn sector_count(&self) -> u64 {
        self.sector_count
    }

    #[must_use]
    pub fn model_bytes(&self) -> &[u8] {
        let length = self.model_length as usize;
        &self.model[..length]
    }

    #[must_use]
    pub const fn is_slave(&self) -> bool {
        self.slave
    }

    fn validate_transfer(&self, lba: u64, length: usize) -> Result<u64, StorageError> {
        if length == 0 || !length.is_multiple_of(SECTOR_SIZE) {
            return Err(StorageError::InvalidBuffer);
        }
        let sectors = u64::try_from(length / SECTOR_SIZE).map_err(|_| StorageError::TooLarge)?;
        if lba
            .checked_add(sectors)
            .is_none_or(|end| end > self.sector_count)
        {
            return Err(StorageError::OutOfBounds);
        }
        Ok(sectors)
    }

    fn read_transfer(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, output.len())?;
        let mut completed = 0_u64;
        while completed < sectors {
            let remaining = sectors - completed;
            let chunk = remaining.min(255);
            let chunk_u8 = u8::try_from(chunk).map_err(|_| StorageError::TooLarge)?;
            let chunk_lba = lba
                .checked_add(completed)
                .ok_or(StorageError::OutOfBounds)?;
            if !self.lba48 && (chunk_lba > 0x0fff_ffff || chunk_lba + chunk > 0x1000_0000) {
                return Err(StorageError::OutOfBounds);
            }
            self.select_and_issue(chunk_lba, chunk_u8, false)?;

            for sector_index in 0..usize::from(chunk_u8) {
                self.wait_data_request()?;
                let global_sector = usize::try_from(completed)
                    .map_err(|_| StorageError::TooLarge)?
                    .checked_add(sector_index)
                    .ok_or(StorageError::TooLarge)?;
                let start = global_sector
                    .checked_mul(SECTOR_SIZE)
                    .ok_or(StorageError::TooLarge)?;
                let sector = &mut output[start..start + SECTOR_SIZE];
                // SAFETY: The ATA data port is owned by this single-core
                // polling driver and the destination is a live sector slice.
                unsafe { in_words(self.io_base, sector) };
                self.delay_400ns();
            }
            completed += chunk;
        }
        Ok(())
    }

    fn write_transfer(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, input.len())?;
        let mut completed = 0_u64;
        while completed < sectors {
            let remaining = sectors - completed;
            let chunk = remaining.min(255);
            let chunk_u8 = u8::try_from(chunk).map_err(|_| StorageError::TooLarge)?;
            let chunk_lba = lba
                .checked_add(completed)
                .ok_or(StorageError::OutOfBounds)?;
            if !self.lba48 && (chunk_lba > 0x0fff_ffff || chunk_lba + chunk > 0x1000_0000) {
                return Err(StorageError::OutOfBounds);
            }
            self.select_and_issue(chunk_lba, chunk_u8, true)?;
            for sector_index in 0..usize::from(chunk_u8) {
                self.wait_data_request()?;
                let global_sector = usize::try_from(completed)
                    .map_err(|_| StorageError::TooLarge)?
                    .checked_add(sector_index)
                    .ok_or(StorageError::TooLarge)?;
                let start = global_sector
                    .checked_mul(SECTOR_SIZE)
                    .ok_or(StorageError::TooLarge)?;
                let sector = &input[start..start + SECTOR_SIZE];
                // SAFETY: The ATA data port is owned by this single-core
                // polling driver and the source is exactly one sector.
                unsafe { out_words(self.io_base, sector) };
                self.delay_400ns();
            }
            completed += chunk;
        }
        Ok(())
    }

    fn select_and_issue(&self, lba: u64, count: u8, write: bool) -> Result<(), StorageError> {
        self.wait_not_busy()?;
        let lba_bytes = lba.to_le_bytes();
        // SAFETY: These are the command registers for the selected IDE
        // channel, discovered from PCI or the legacy fixed assignments.
        unsafe {
            outb(
                self.io_base + 6,
                if self.lba48 {
                    0x40 | (u8::from(self.slave) << 4)
                } else {
                    0xe0 | (u8::from(self.slave) << 4) | (lba_bytes[3] & 0x0f)
                },
            );
            if self.lba48 {
                outb(self.io_base + 2, 0);
                outb(self.io_base + 3, lba_bytes[3]);
                outb(self.io_base + 4, lba_bytes[4]);
                outb(self.io_base + 5, lba_bytes[5]);
            }
            outb(self.io_base + 2, count);
            outb(self.io_base + 3, lba_bytes[0]);
            outb(self.io_base + 4, lba_bytes[1]);
            outb(self.io_base + 5, lba_bytes[2]);
            let command = match (self.lba48, write) {
                (true, false) => 0x24,
                (true, true) => 0x34,
                (false, false) => 0x20,
                (false, true) => 0x30,
            };
            outb(self.io_base + 7, command);
        }
        Ok(())
    }

    fn wait_not_busy(&self) -> Result<u8, StorageError> {
        for _ in 0..POLL_LIMIT {
            // SAFETY: Reading the owned channel status register is harmless.
            let status = unsafe { inb(self.io_base + 7) };
            if status & STATUS_BSY == 0 {
                if status & (STATUS_ERR | STATUS_DF) != 0 {
                    return Err(StorageError::Device);
                }
                return Ok(status);
            }
            spin_loop();
        }
        Err(StorageError::Timeout)
    }

    fn wait_data_request(&self) -> Result<(), StorageError> {
        for _ in 0..POLL_LIMIT {
            // SAFETY: Reading the owned channel status register is harmless.
            let status = unsafe { inb(self.io_base + 7) };
            if status & STATUS_BSY == 0 {
                if status & (STATUS_ERR | STATUS_DF) != 0 {
                    return Err(StorageError::Device);
                }
                if status & STATUS_DRQ != 0 {
                    return Ok(());
                }
            }
            spin_loop();
        }
        Err(StorageError::Timeout)
    }

    fn delay_400ns(&self) {
        for _ in 0..4 {
            // SAFETY: The alternate-status port is read-only for this access.
            let _ = unsafe { inb(self.control) };
        }
    }
}

impl BlockDevice for IdeDevice {
    fn sector_size(&self) -> u32 {
        512
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        self.read_transfer(lba, output)
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        self.write_transfer(lba, input)
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        self.wait_not_busy()?;
        // SAFETY: This writes ATA FLUSH CACHE to the selected drive.
        unsafe {
            outb(self.io_base + 6, 0x40 | (u8::from(self.slave) << 4));
            outb(self.io_base + 7, if self.lba48 { 0xea } else { 0xe7 });
        }
        self.wait_not_busy().map(|_| ())
    }
}

pub fn discover(inventory: &PciInventory, output: &mut [Option<IdeDevice>]) -> usize {
    let mut count = 0;
    let mut found_controller = false;
    for device in inventory.devices() {
        if device.class != 0x01 || device.subclass != 0x01 {
            continue;
        }
        found_controller = true;
        device.enable_io_space();
        let primary = channel_ports(*device, true);
        let secondary = channel_ports(*device, false);
        count += probe_channel(primary.0, primary.1, &mut output[count..]);
        if count == output.len() {
            return count;
        }
        count += probe_channel(secondary.0, secondary.1, &mut output[count..]);
        if count == output.len() {
            return count;
        }
    }
    if !found_controller {
        count += probe_channel(0x1f0, 0x3f6, &mut output[count..]);
        if count < output.len() {
            count += probe_channel(0x170, 0x376, &mut output[count..]);
        }
    }
    count
}

fn channel_ports(device: PciDevice, primary: bool) -> (u16, u16) {
    let native = if primary {
        device.programming_interface & 1 != 0
    } else {
        device.programming_interface & 4 != 0
    };
    if native {
        let command_index = if primary { 0 } else { 2 };
        let control_index = command_index + 1;
        let command = device
            .bar(command_index)
            .filter(|bar| bar.is_io)
            .and_then(|bar| u16::try_from(bar.address).ok());
        let control = device
            .bar(control_index)
            .filter(|bar| bar.is_io)
            .and_then(|bar| u16::try_from(bar.address).ok())
            .and_then(|base| base.checked_add(2));
        if let (Some(command), Some(control)) = (command, control) {
            return (command, control);
        }
    }
    if primary {
        (0x1f0, 0x3f6)
    } else {
        (0x170, 0x376)
    }
}

fn probe_channel(io_base: u16, control: u16, output: &mut [Option<IdeDevice>]) -> usize {
    let mut count = 0;
    for slave in [false, true] {
        if count == output.len() {
            break;
        }
        if let Some(device) = identify(io_base, control, slave) {
            output[count] = Some(device);
            count += 1;
        }
    }
    count
}

fn identify(io_base: u16, control: u16, slave: bool) -> Option<IdeDevice> {
    // SAFETY: This performs the standard non-destructive ATA IDENTIFY
    // transaction against one channel/device selection.
    unsafe {
        outb(control, 2);
        outb(io_base + 6, 0xa0 | (u8::from(slave) << 4));
        for _ in 0..4 {
            let _ = inb(control);
        }
        outb(io_base + 2, 0);
        outb(io_base + 3, 0);
        outb(io_base + 4, 0);
        outb(io_base + 5, 0);
        outb(io_base + 7, 0xec);
        if inb(io_base + 7) == 0 {
            return None;
        }
        for _ in 0..POLL_LIMIT {
            let status = inb(io_base + 7);
            if status & STATUS_BSY != 0 {
                spin_loop();
                continue;
            }
            if inb(io_base + 4) != 0 || inb(io_base + 5) != 0 {
                return None;
            }
            if status & (STATUS_ERR | STATUS_DF) != 0 {
                return None;
            }
            if status & STATUS_DRQ != 0 {
                let mut words = [0_u16; 256];
                for word in &mut words {
                    *word = inw(io_base);
                }
                return parse_identify(io_base, control, slave, &words);
            }
        }
    }
    None
}

fn parse_identify(
    io_base: u16,
    control: u16,
    slave: bool,
    words: &[u16; 256],
) -> Option<IdeDevice> {
    let lba48 = words[83] & (1 << 10) != 0;
    let sector_count = if lba48 {
        u64::from(words[100])
            | (u64::from(words[101]) << 16)
            | (u64::from(words[102]) << 32)
            | (u64::from(words[103]) << 48)
    } else {
        u64::from(words[60]) | (u64::from(words[61]) << 16)
    };
    if sector_count == 0 {
        return None;
    }
    let mut model = [b' '; 40];
    for (index, word) in words[27..47].iter().copied().enumerate() {
        let bytes = word.to_be_bytes();
        model[index * 2] = bytes[0];
        model[index * 2 + 1] = bytes[1];
    }
    let model_length = model
        .iter()
        .rposition(|byte| *byte != b' ' && *byte != 0)
        .map_or(0, |index| index + 1);
    Some(IdeDevice {
        io_base,
        control,
        slave,
        sector_count,
        lba48,
        model,
        model_length: u8::try_from(model_length).ok()?,
    })
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Caller owns the I/O port.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    }
    value
}

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller owns the I/O port.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: Caller owns the I/O port.
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

unsafe fn in_words(port: u16, output: &mut [u8]) {
    for chunk in output.as_chunks_mut::<2>().0 {
        // SAFETY: The caller owns the ATA data port.
        let value = unsafe { inw(port) }.to_le_bytes();
        chunk.copy_from_slice(&value);
    }
}

unsafe fn out_words(port: u16, input: &[u8]) {
    for chunk in input.as_chunks::<2>().0 {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        // SAFETY: The caller owns the ATA data port.
        unsafe {
            asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
        }
    }
}
