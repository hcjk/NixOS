use core::hint::spin_loop;

use nexos_storage::{BlockDevice, StorageError};

use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};
use crate::pci::PciInventory;

const PAGE_SIZE: u64 = 4096;
const PAGE_SIZE_BYTES: usize = 4096;
const AHCI_MMIO_SIZE: u64 = 0x2000;
const AHCI_MMIO_BASE: u64 = 0xffff_ff00_1000_0000;
const AHCI_MMIO_STRIDE: u64 = 0x20_0000;
const SATA_SIGNATURE: u32 = 0x0000_0101;
const COMMAND_TABLE_BYTES: usize = 256;
const DMA_PAGES: u64 = 2;
const DMA_BYTES: usize = PAGE_SIZE_BYTES * 2;
const SECTOR_SIZE: usize = 512;
const MAX_SECTORS_PER_COMMAND: usize = DMA_BYTES / SECTOR_SIZE;
const POLL_LIMIT: usize = 4_000_000;

const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0c;
const HBA_VS: u64 = 0x10;
const HBA_CAP2: u64 = 0x24;
const HBA_BOHC: u64 = 0x28;
const GHC_AE: u32 = 1 << 31;

const PORT_CLB: u64 = 0x00;
const PORT_CLBU: u64 = 0x04;
const PORT_FB: u64 = 0x08;
const PORT_FBU: u64 = 0x0c;
const PORT_IS: u64 = 0x10;
const PORT_IE: u64 = 0x14;
const PORT_CMD: u64 = 0x18;
const PORT_TFD: u64 = 0x20;
const PORT_SIG: u64 = 0x24;
const PORT_SSTS: u64 = 0x28;
const PORT_SERR: u64 = 0x30;
const PORT_SACT: u64 = 0x34;
const PORT_CI: u64 = 0x38;
const PORT_CMD_ST: u32 = 1;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_CMD_FR: u32 = 1 << 14;
const PORT_CMD_CR: u32 = 1 << 15;
const PORT_IS_TFES: u32 = 1 << 30;
const ATA_STATUS_ERR: u32 = 1;
const ATA_STATUS_DRQ: u32 = 1 << 3;
const ATA_STATUS_BSY: u32 = 1 << 7;

#[derive(Clone, Copy, Default)]
pub struct AhciProbeStats {
    pub controllers: u8,
    pub bars: u8,
    pub mapped_controllers: u8,
    pub last_abar: u64,
    pub map_error: Option<PageMapError>,
    pub implemented_ports: u8,
    pub sata_ports: u8,
    pub initialized_ports: u8,
    pub identify_failures: u8,
}

#[derive(Clone, Copy)]
pub struct AhciDevice {
    port: u64,
    command_list_virtual: u64,
    command_table_physical: u64,
    command_table_virtual: u64,
    dma_physical: u64,
    dma_virtual: u64,
    sector_count: u64,
    lba48: bool,
    model: [u8; 40],
    model_length: u8,
    controller_version: u32,
    port_number: u8,
}

impl AhciDevice {
    #[must_use]
    pub const fn sector_count(&self) -> u64 {
        self.sector_count
    }

    #[must_use]
    pub fn model_bytes(&self) -> &[u8] {
        &self.model[..usize::from(self.model_length)]
    }

    #[must_use]
    pub const fn port_number(&self) -> u8 {
        self.port_number
    }

    #[must_use]
    pub const fn controller_version(&self) -> u32 {
        self.controller_version
    }

    fn identify(&mut self) -> Result<(), StorageError> {
        self.issue(0xec, 0, 0, false, SECTOR_SIZE)?;
        // SAFETY: The DMA buffer is exclusively owned by this port and the
        // completed command transferred a 512-byte IDENTIFY block into it.
        let words = unsafe { core::slice::from_raw_parts(self.dma_virtual as *const u16, 256) };
        self.lba48 = words[83] & (1 << 10) != 0;
        self.sector_count = if self.lba48 {
            u64::from(words[100])
                | (u64::from(words[101]) << 16)
                | (u64::from(words[102]) << 32)
                | (u64::from(words[103]) << 48)
        } else {
            u64::from(words[60]) | (u64::from(words[61]) << 16)
        };
        if self.sector_count == 0 {
            return Err(StorageError::Device);
        }
        for (index, word) in words[27..47].iter().copied().enumerate() {
            let bytes = word.to_be_bytes();
            self.model[index * 2] = bytes[0];
            self.model[index * 2 + 1] = bytes[1];
        }
        self.model_length = u8::try_from(
            self.model
                .iter()
                .rposition(|byte| *byte != b' ' && *byte != 0)
                .map_or(0, |index| index + 1),
        )
        .map_err(|_| StorageError::TooLarge)?;
        Ok(())
    }

    fn issue(
        &mut self,
        ata_command: u8,
        lba: u64,
        sector_count: u16,
        write: bool,
        byte_count: usize,
    ) -> Result<(), StorageError> {
        if byte_count > DMA_BYTES || (byte_count != 0 && !byte_count.is_multiple_of(SECTOR_SIZE)) {
            return Err(StorageError::InvalidBuffer);
        }
        for _ in 0..POLL_LIMIT {
            let task = read32(self.port + PORT_TFD);
            if task & (ATA_STATUS_BSY | ATA_STATUS_DRQ) == 0 {
                break;
            }
            spin_loop();
        }
        if read32(self.port + PORT_TFD) & (ATA_STATUS_BSY | ATA_STATUS_DRQ) != 0 {
            return Err(StorageError::Timeout);
        }

        // Slot zero is the only command slot used by this early polling
        // driver. Clear its header and table before constructing the request.
        // SAFETY: Both allocations are exclusive, correctly aligned DMA
        // regions allocated during port setup.
        unsafe {
            core::ptr::write_bytes(self.command_list_virtual as *mut u8, 0, 32);
            core::ptr::write_bytes(
                self.command_table_virtual as *mut u8,
                0,
                COMMAND_TABLE_BYTES,
            );
        }
        let mut header_flags = 5_u32;
        if write {
            header_flags |= 1 << 6;
        }
        if byte_count != 0 {
            header_flags |= 1 << 16;
        }
        write_memory32(self.command_list_virtual, header_flags);
        let (command_table_low, command_table_high) = split_address(self.command_table_physical);
        write_memory32(self.command_list_virtual + 8, command_table_low);
        write_memory32(self.command_list_virtual + 12, command_table_high);

        let fis = self.command_table_virtual;
        let lba_bytes = lba.to_le_bytes();
        let sector_count_bytes = sector_count.to_le_bytes();
        write_memory8(fis, 0x27);
        write_memory8(fis + 1, 1 << 7);
        write_memory8(fis + 2, ata_command);
        write_memory8(fis + 7, 1 << 6);
        write_memory8(fis + 4, lba_bytes[0]);
        write_memory8(fis + 5, lba_bytes[1]);
        write_memory8(fis + 6, lba_bytes[2]);
        write_memory8(fis + 8, lba_bytes[3]);
        write_memory8(fis + 9, lba_bytes[4]);
        write_memory8(fis + 10, lba_bytes[5]);
        write_memory8(fis + 12, sector_count_bytes[0]);
        write_memory8(fis + 13, sector_count_bytes[1]);

        if byte_count != 0 {
            let prdt = self.command_table_virtual + 128;
            let (dma_low, dma_high) = split_address(self.dma_physical);
            write_memory32(prdt, dma_low);
            write_memory32(prdt + 4, dma_high);
            write_memory32(
                prdt + 12,
                u32::try_from(byte_count - 1).map_err(|_| StorageError::TooLarge)? | (1 << 31),
            );
        }

        write32(self.port + PORT_IS, u32::MAX);
        write32(self.port + PORT_SACT, 0);
        write32(self.port + PORT_CI, 1);
        for _ in 0..POLL_LIMIT {
            let interrupt_status = read32(self.port + PORT_IS);
            if interrupt_status & PORT_IS_TFES != 0 {
                return Err(StorageError::Device);
            }
            if read32(self.port + PORT_CI) & 1 == 0 {
                let task = read32(self.port + PORT_TFD);
                return if task & ATA_STATUS_ERR == 0 {
                    Ok(())
                } else {
                    Err(StorageError::Device)
                };
            }
            spin_loop();
        }
        Err(StorageError::Timeout)
    }

    fn validate_transfer(&self, lba: u64, length: usize) -> Result<usize, StorageError> {
        if length == 0 || !length.is_multiple_of(SECTOR_SIZE) {
            return Err(StorageError::InvalidBuffer);
        }
        let sectors = length / SECTOR_SIZE;
        let sectors_u64 = u64::try_from(sectors).map_err(|_| StorageError::TooLarge)?;
        if lba
            .checked_add(sectors_u64)
            .is_none_or(|end| end > self.sector_count)
        {
            return Err(StorageError::OutOfBounds);
        }
        if !self.lba48
            && lba
                .checked_add(sectors_u64)
                .is_none_or(|end| end > 0x1000_0000)
        {
            return Err(StorageError::OutOfBounds);
        }
        Ok(sectors)
    }

    fn read_transfer(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, output.len())?;
        let mut completed = 0;
        while completed < sectors {
            let chunk = (sectors - completed).min(MAX_SECTORS_PER_COMMAND);
            let byte_count = chunk * SECTOR_SIZE;
            let command = if self.lba48 { 0x25 } else { 0xc8 };
            self.issue(
                command,
                lba + u64::try_from(completed).map_err(|_| StorageError::TooLarge)?,
                u16::try_from(chunk).map_err(|_| StorageError::TooLarge)?,
                false,
                byte_count,
            )?;
            let start = completed * SECTOR_SIZE;
            // SAFETY: The completed DMA command populated byte_count bytes,
            // and output[start..] has the same validated length.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.dma_virtual as *const u8,
                    output[start..start + byte_count].as_mut_ptr(),
                    byte_count,
                );
            }
            completed += chunk;
        }
        Ok(())
    }

    fn write_transfer(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, input.len())?;
        let mut completed = 0;
        while completed < sectors {
            let chunk = (sectors - completed).min(MAX_SECTORS_PER_COMMAND);
            let byte_count = chunk * SECTOR_SIZE;
            let start = completed * SECTOR_SIZE;
            // SAFETY: The exclusive DMA buffer is at least byte_count bytes
            // and the source slice has been bounds-checked.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    input[start..start + byte_count].as_ptr(),
                    self.dma_virtual as *mut u8,
                    byte_count,
                );
            }
            let command = if self.lba48 { 0x35 } else { 0xca };
            self.issue(
                command,
                lba + u64::try_from(completed).map_err(|_| StorageError::TooLarge)?,
                u16::try_from(chunk).map_err(|_| StorageError::TooLarge)?,
                true,
                byte_count,
            )?;
            completed += chunk;
        }
        Ok(())
    }
}

impl BlockDevice for AhciDevice {
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
        self.issue(if self.lba48 { 0xea } else { 0xe7 }, 0, 0, false, 0)
    }
}

pub fn discover(
    inventory: &PciInventory,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
    output: &mut [Option<AhciDevice>],
    stats: &mut AhciProbeStats,
) -> usize {
    let mut count = 0;
    let mut controller_index = 0_u64;
    for device in inventory.devices() {
        if device.class != 0x01 || device.subclass != 0x06 || device.programming_interface != 0x01 {
            continue;
        }
        stats.controllers = stats.controllers.saturating_add(1);
        let Some(bar) = device.bar(5).filter(|bar| !bar.is_io) else {
            continue;
        };
        stats.bars = stats.bars.saturating_add(1);
        stats.last_abar = bar.address;
        let Some(virtual_base) =
            AHCI_MMIO_BASE.checked_add(controller_index.saturating_mul(AHCI_MMIO_STRIDE))
        else {
            continue;
        };
        controller_index += 1;
        let hba = match map_mmio(paging, allocator, bar.address, AHCI_MMIO_SIZE, virtual_base) {
            Ok(hba) => hba,
            Err(error) => {
                stats.map_error = Some(error);
                continue;
            }
        };
        stats.mapped_controllers = stats.mapped_controllers.saturating_add(1);
        device.enable_memory_bus_mastering();
        if !bios_handoff(hba) {
            continue;
        }
        write32(hba + HBA_GHC, read32(hba + HBA_GHC) | GHC_AE);
        let capabilities = read32(hba + HBA_CAP);
        let supports_64_bit = capabilities & (1 << 31) != 0;
        let implemented = read32(hba + HBA_PI);
        let version = read32(hba + HBA_VS);
        for port_number in 0_u8..32 {
            if implemented & (1_u32 << port_number) == 0 || count == output.len() {
                continue;
            }
            stats.implemented_ports = stats.implemented_ports.saturating_add(1);
            let port = hba + 0x100 + u64::from(port_number) * 0x80;
            let link_status = read32(port + PORT_SSTS);
            if link_status & 0x0f != 3 || (link_status >> 8) & 0x0f != 1 {
                continue;
            }
            if read32(port + PORT_SIG) != SATA_SIGNATURE {
                continue;
            }
            stats.sata_ports = stats.sata_ports.saturating_add(1);
            if let Some(mut disk) = initialize_port(
                port,
                port_number,
                version,
                supports_64_bit,
                paging.hhdm_offset(),
                allocator,
            ) {
                stats.initialized_ports = stats.initialized_ports.saturating_add(1);
                if disk.identify().is_ok() {
                    output[count] = Some(disk);
                    count += 1;
                } else {
                    stats.identify_failures = stats.identify_failures.saturating_add(1);
                    stop_port(port);
                }
            }
        }
    }
    count
}

fn initialize_port(
    port: u64,
    port_number: u8,
    controller_version: u32,
    supports_64_bit: bool,
    hhdm_offset: u64,
    allocator: &mut FrameAllocator,
) -> Option<AhciDevice> {
    if !stop_port(port) {
        return None;
    }
    write32(port + PORT_SERR, u32::MAX);
    write32(port + PORT_IS, u32::MAX);
    write32(port + PORT_IE, 0);

    let command_list_physical = allocator.allocate()?;
    let fis_physical = allocator.allocate()?;
    let command_table_physical = allocator.allocate()?;
    let dma_physical = allocator.allocate_contiguous(DMA_PAGES)?;
    if !supports_64_bit
        && [
            command_list_physical,
            fis_physical,
            command_table_physical,
            dma_physical,
        ]
        .into_iter()
        .any(|address| address > u64::from(u32::MAX))
    {
        return None;
    }
    let command_list_virtual = hhdm_offset.checked_add(command_list_physical)?;
    let fis_virtual = hhdm_offset.checked_add(fis_physical)?;
    let command_table_virtual = hhdm_offset.checked_add(command_table_physical)?;
    let dma_virtual = hhdm_offset.checked_add(dma_physical)?;
    // SAFETY: The frame allocator granted exclusive ownership of all regions.
    unsafe {
        core::ptr::write_bytes(command_list_virtual as *mut u8, 0, PAGE_SIZE_BYTES);
        core::ptr::write_bytes(fis_virtual as *mut u8, 0, PAGE_SIZE_BYTES);
        core::ptr::write_bytes(command_table_virtual as *mut u8, 0, PAGE_SIZE_BYTES);
        core::ptr::write_bytes(dma_virtual as *mut u8, 0, DMA_BYTES);
    }
    let (command_list_low, command_list_high) = split_address(command_list_physical);
    let (fis_low, fis_high) = split_address(fis_physical);
    write32(port + PORT_CLB, command_list_low);
    write32(port + PORT_CLBU, command_list_high);
    write32(port + PORT_FB, fis_low);
    write32(port + PORT_FBU, fis_high);
    if !start_port(port) {
        return None;
    }
    Some(AhciDevice {
        port,
        command_list_virtual,
        command_table_physical,
        command_table_virtual,
        dma_physical,
        dma_virtual,
        sector_count: 0,
        lba48: false,
        model: [b' '; 40],
        model_length: 0,
        controller_version,
        port_number,
    })
}

fn bios_handoff(hba: u64) -> bool {
    if read32(hba + HBA_CAP2) & 1 == 0 {
        return true;
    }
    let mut control = read32(hba + HBA_BOHC);
    control |= 1 << 1;
    write32(hba + HBA_BOHC, control);
    for _ in 0..POLL_LIMIT {
        control = read32(hba + HBA_BOHC);
        if control & ((1 << 0) | (1 << 4)) == 0 {
            return true;
        }
        spin_loop();
    }
    false
}

fn stop_port(port: u64) -> bool {
    let mut command = read32(port + PORT_CMD);
    command &= !(PORT_CMD_ST | PORT_CMD_FRE);
    write32(port + PORT_CMD, command);
    for _ in 0..POLL_LIMIT {
        if read32(port + PORT_CMD) & (PORT_CMD_CR | PORT_CMD_FR) == 0 {
            return true;
        }
        spin_loop();
    }
    false
}

fn start_port(port: u64) -> bool {
    for _ in 0..POLL_LIMIT {
        if read32(port + PORT_CMD) & PORT_CMD_CR == 0 {
            let command = read32(port + PORT_CMD) | PORT_CMD_FRE | PORT_CMD_ST;
            write32(port + PORT_CMD, command);
            return true;
        }
        spin_loop();
    }
    false
}

fn map_mmio(
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
    physical: u64,
    length: u64,
    virtual_base: u64,
) -> Result<u64, PageMapError> {
    let physical_page = physical & !(PAGE_SIZE - 1);
    let offset = physical - physical_page;
    let bytes = offset
        .checked_add(length)
        .ok_or(PageMapError::AddressOverflow)?;
    let pages = bytes.div_ceil(PAGE_SIZE);
    for page in 0..pages {
        paging.map_mmio_page(
            virtual_base + page * PAGE_SIZE,
            physical_page + page * PAGE_SIZE,
            allocator,
        )?;
    }
    virtual_base
        .checked_add(offset)
        .ok_or(PageMapError::AddressOverflow)
}

fn read32(address: u64) -> u32 {
    // SAFETY: Callers pass aligned AHCI MMIO registers in a mapped controller
    // aperture.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write32(address: u64, value: u32) {
    // SAFETY: Callers pass aligned writable AHCI MMIO registers.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn write_memory32(address: u64, value: u32) {
    // SAFETY: Callers pass aligned addresses in exclusive DMA allocations.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn write_memory8(address: u64, value: u8) {
    // SAFETY: Callers pass addresses in an exclusive command-table allocation.
    unsafe { core::ptr::write_volatile(address as *mut u8, value) };
}

fn split_address(address: u64) -> (u32, u32) {
    let bytes = address.to_le_bytes();
    (
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    )
}
