use core::hint::spin_loop;
use core::sync::atomic::{Ordering, fence};

use nexos_storage::{BlockDevice, StorageError};

use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};
use crate::pci::PciInventory;

const PAGE_SIZE: u64 = 4096;
const PAGE_BYTES: usize = 4096;
const NVME_MMIO_BYTES: u64 = 0x4000;
const NVME_MMIO_BASE: u64 = 0xffff_ff00_2000_0000;
const NVME_MMIO_STRIDE: u64 = 0x20_0000;
const QUEUE_DEPTH: u16 = 16;
const POLL_LIMIT: usize = 12_000_000;

const REG_CAP: u64 = 0x00;
const REG_VERSION: u64 = 0x08;
const REG_INTERRUPT_MASK_SET: u64 = 0x0c;
const REG_CONTROLLER_CONFIG: u64 = 0x14;
const REG_CONTROLLER_STATUS: u64 = 0x1c;
const REG_ADMIN_QUEUE_ATTRIBUTES: u64 = 0x24;
const REG_ADMIN_SUBMISSION_QUEUE: u64 = 0x28;
const REG_ADMIN_COMPLETION_QUEUE: u64 = 0x30;
const REG_DOORBELLS: u64 = 0x1000;

const CC_ENABLE: u32 = 1;
const CSTS_READY: u32 = 1;
const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

#[derive(Clone, Copy, Default)]
pub struct NvmeProbeStats {
    pub controllers: u8,
    pub mapped_controllers: u8,
    pub initialized_namespaces: u8,
    pub unsupported_controllers: u8,
    pub initialization_failures: u8,
    pub last_bar: u64,
    pub map_error: Option<PageMapError>,
}

#[derive(Clone, Copy)]
struct Queue {
    submission_physical: u64,
    submission_virtual: u64,
    completion_physical: u64,
    completion_virtual: u64,
    depth: u16,
    id: u16,
    submission_tail: u16,
    completion_head: u16,
    completion_phase: bool,
    next_command_id: u16,
}

impl Queue {
    fn reset(&mut self) {
        self.submission_tail = 0;
        self.completion_head = 0;
        self.completion_phase = true;
        self.next_command_id = 1;
        // SAFETY: Each queue owns one exclusive page allocated during probe.
        unsafe {
            core::ptr::write_bytes(self.submission_virtual as *mut u8, 0, PAGE_BYTES);
            core::ptr::write_bytes(self.completion_virtual as *mut u8, 0, PAGE_BYTES);
        }
    }

    fn submit(
        &mut self,
        registers: u64,
        doorbell_stride: u64,
        mut command: [u32; 16],
    ) -> Result<(), StorageError> {
        let command_id = self.next_command_id;
        self.next_command_id = self.next_command_id.wrapping_add(1).max(1);
        command[0] = (command[0] & 0xffff) | (u32::from(command_id) << 16);
        let submission = self.submission_virtual + u64::from(self.submission_tail) * 64;
        for (index, value) in command.into_iter().enumerate() {
            write_memory32(submission + u64::try_from(index).unwrap_or(0) * 4, value);
        }
        self.submission_tail = (self.submission_tail + 1) % self.depth;
        fence(Ordering::SeqCst);
        write32(
            doorbell(registers, self.id, false, doorbell_stride),
            u32::from(self.submission_tail),
        );

        let completion = self.completion_virtual + u64::from(self.completion_head) * 16;
        for _ in 0..POLL_LIMIT {
            let status_word = read_memory32(completion + 12);
            if (status_word & (1 << 16) != 0) == self.completion_phase {
                fence(Ordering::SeqCst);
                let completed_id = status_word.to_le_bytes();
                let completed_id = u16::from_le_bytes([completed_id[0], completed_id[1]]);
                let status = (status_word >> 17) & 0x7fff;
                self.completion_head += 1;
                if self.completion_head == self.depth {
                    self.completion_head = 0;
                    self.completion_phase = !self.completion_phase;
                }
                write32(
                    doorbell(registers, self.id, true, doorbell_stride),
                    u32::from(self.completion_head),
                );
                return if completed_id == command_id && status == 0 {
                    Ok(())
                } else {
                    Err(StorageError::Device)
                };
            }
            spin_loop();
        }
        Err(StorageError::Timeout)
    }
}

#[derive(Clone, Copy)]
pub struct NvmeDevice {
    registers: u64,
    capabilities: u64,
    version: u32,
    doorbell_stride: u64,
    admin: Queue,
    io: Queue,
    data_physical: u64,
    data_virtual: u64,
    namespace_id: u32,
    sector_count: u64,
    sector_size: u32,
    model: [u8; 40],
    model_length: u8,
    controller_index: u8,
    recovery_count: u32,
}

impl NvmeDevice {
    #[must_use]
    pub const fn sector_count(&self) -> u64 {
        self.sector_count
    }

    #[must_use]
    pub const fn sector_size(&self) -> u32 {
        self.sector_size
    }

    #[must_use]
    pub const fn controller_version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn namespace_id(&self) -> u32 {
        self.namespace_id
    }

    #[must_use]
    pub const fn controller_index(&self) -> u8 {
        self.controller_index
    }

    #[must_use]
    pub const fn recovery_count(&self) -> u32 {
        self.recovery_count
    }

    #[must_use]
    pub fn model_bytes(&self) -> &[u8] {
        &self.model[..usize::from(self.model_length)]
    }

    pub fn recover(&mut self) -> Result<(), StorageError> {
        configure_controller(
            self.registers,
            self.capabilities,
            &mut self.admin,
            &mut self.io,
            self.doorbell_stride,
        )?;
        self.recovery_count = self.recovery_count.saturating_add(1);
        Ok(())
    }

    fn submit_io(&mut self, command: [u32; 16]) -> Result<(), StorageError> {
        if self
            .io
            .submit(self.registers, self.doorbell_stride, command)
            .is_ok()
        {
            return Ok(());
        }
        self.recover()?;
        self.io
            .submit(self.registers, self.doorbell_stride, command)
    }

    fn validate_transfer(&self, lba: u64, length: usize) -> Result<usize, StorageError> {
        let sector_size = usize::try_from(self.sector_size).map_err(|_| StorageError::TooLarge)?;
        if length == 0 || !length.is_multiple_of(sector_size) {
            return Err(StorageError::InvalidBuffer);
        }
        let sectors = length / sector_size;
        let sector_count = u64::try_from(sectors).map_err(|_| StorageError::TooLarge)?;
        if lba
            .checked_add(sector_count)
            .is_none_or(|end| end > self.sector_count)
        {
            return Err(StorageError::OutOfBounds);
        }
        Ok(sectors)
    }

    fn submit_data_command(
        &mut self,
        lba: u64,
        sector_count: usize,
        write: bool,
    ) -> Result<(), StorageError> {
        let mut command = [0_u32; 16];
        command[0] = u32::from(if write { IO_WRITE } else { IO_READ });
        command[1] = self.namespace_id;
        set_prp(&mut command, self.data_physical);
        let lba_bytes = lba.to_le_bytes();
        command[10] = u32::from_le_bytes([lba_bytes[0], lba_bytes[1], lba_bytes[2], lba_bytes[3]]);
        command[11] = u32::from_le_bytes([lba_bytes[4], lba_bytes[5], lba_bytes[6], lba_bytes[7]]);
        command[12] = u32::try_from(sector_count - 1).map_err(|_| StorageError::TooLarge)?;
        self.submit_io(command)
    }
}

impl BlockDevice for NvmeDevice {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, output.len())?;
        let sector_size = usize::try_from(self.sector_size).map_err(|_| StorageError::TooLarge)?;
        let maximum_sectors = PAGE_BYTES / sector_size;
        let mut completed = 0_usize;
        while completed < sectors {
            let count = (sectors - completed).min(maximum_sectors);
            let byte_offset = completed * sector_size;
            let byte_count = count * sector_size;
            let command_lba = lba
                .checked_add(u64::try_from(completed).map_err(|_| StorageError::TooLarge)?)
                .ok_or(StorageError::OutOfBounds)?;
            self.submit_data_command(command_lba, count, false)?;
            // SAFETY: Successful completion makes this exclusive DMA range
            // available to the validated output slice.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data_virtual as *const u8,
                    output[byte_offset..byte_offset + byte_count].as_mut_ptr(),
                    byte_count,
                );
            }
            completed += count;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        let sectors = self.validate_transfer(lba, input.len())?;
        let sector_size = usize::try_from(self.sector_size).map_err(|_| StorageError::TooLarge)?;
        let maximum_sectors = PAGE_BYTES / sector_size;
        let mut completed = 0_usize;
        while completed < sectors {
            let count = (sectors - completed).min(maximum_sectors);
            let byte_offset = completed * sector_size;
            let byte_count = count * sector_size;
            // SAFETY: The DMA page is exclusive and both ranges are valid.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    input[byte_offset..byte_offset + byte_count].as_ptr(),
                    self.data_virtual as *mut u8,
                    byte_count,
                );
            }
            let command_lba = lba
                .checked_add(u64::try_from(completed).map_err(|_| StorageError::TooLarge)?)
                .ok_or(StorageError::OutOfBounds)?;
            self.submit_data_command(command_lba, count, true)?;
            completed += count;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        let mut command = [0_u32; 16];
        command[0] = u32::from(IO_FLUSH);
        command[1] = self.namespace_id;
        self.submit_io(command)
    }
}

pub fn discover(
    inventory: &PciInventory,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
    output: &mut [Option<NvmeDevice>],
    stats: &mut NvmeProbeStats,
) -> usize {
    let mut count = 0_usize;
    for device in inventory
        .devices()
        .iter()
        .filter(|device| device.class == 0x01 && device.subclass == 0x08)
    {
        stats.controllers = stats.controllers.saturating_add(1);
        let Some(bar) = device.bar(0).filter(|bar| !bar.is_io && bar.address != 0) else {
            stats.unsupported_controllers = stats.unsupported_controllers.saturating_add(1);
            continue;
        };
        stats.last_bar = bar.address;
        let controller_index = u64::from(stats.controllers - 1);
        let Some(virtual_base) = NVME_MMIO_BASE.checked_add(controller_index * NVME_MMIO_STRIDE)
        else {
            stats.initialization_failures = stats.initialization_failures.saturating_add(1);
            continue;
        };
        let registers = match map_mmio(
            paging,
            allocator,
            bar.address,
            NVME_MMIO_BYTES,
            virtual_base,
        ) {
            Ok(registers) => registers,
            Err(error) => {
                stats.map_error = Some(error);
                stats.initialization_failures = stats.initialization_failures.saturating_add(1);
                continue;
            }
        };
        stats.mapped_controllers = stats.mapped_controllers.saturating_add(1);
        device.enable_memory_bus_mastering();
        match initialize(
            registers,
            stats.controllers - 1,
            allocator,
            paging.hhdm_offset(),
        ) {
            Ok(namespace) if count < output.len() => {
                output[count] = Some(namespace);
                count += 1;
                stats.initialized_namespaces = stats.initialized_namespaces.saturating_add(1);
            }
            Ok(_) => break,
            Err(StorageError::UnsupportedSectorSize) => {
                stats.unsupported_controllers = stats.unsupported_controllers.saturating_add(1);
            }
            Err(_) => {
                stats.initialization_failures = stats.initialization_failures.saturating_add(1);
            }
        }
    }
    count
}

fn initialize(
    registers: u64,
    controller_index: u8,
    allocator: &mut FrameAllocator,
    hhdm_offset: u64,
) -> Result<NvmeDevice, StorageError> {
    let capabilities = read64(registers + REG_CAP);
    let maximum_entries = (capabilities & 0xffff).saturating_add(1);
    let minimum_page_shift = (capabilities >> 48) & 0xf;
    let supports_nvm = capabilities & (1 << 37) != 0;
    if maximum_entries < u64::from(QUEUE_DEPTH) || minimum_page_shift != 0 || !supports_nvm {
        return Err(StorageError::UnsupportedSectorSize);
    }
    let doorbell_stride = 4_u64 << ((capabilities >> 32) & 0xf);
    let allocate_queue = |allocator: &mut FrameAllocator, id| -> Result<Queue, StorageError> {
        let submission_physical = allocator.allocate().ok_or(StorageError::NoSpace)?;
        let completion_physical = allocator.allocate().ok_or(StorageError::NoSpace)?;
        Ok(Queue {
            submission_physical,
            submission_virtual: hhdm_offset
                .checked_add(submission_physical)
                .ok_or(StorageError::TooLarge)?,
            completion_physical,
            completion_virtual: hhdm_offset
                .checked_add(completion_physical)
                .ok_or(StorageError::TooLarge)?,
            depth: QUEUE_DEPTH,
            id,
            submission_tail: 0,
            completion_head: 0,
            completion_phase: true,
            next_command_id: 1,
        })
    };
    let mut admin = allocate_queue(allocator, 0)?;
    let mut io = allocate_queue(allocator, 1)?;
    let data_physical = allocator.allocate().ok_or(StorageError::NoSpace)?;
    let data_virtual = hhdm_offset
        .checked_add(data_physical)
        .ok_or(StorageError::TooLarge)?;
    configure_controller(
        registers,
        capabilities,
        &mut admin,
        &mut io,
        doorbell_stride,
    )?;

    let mut identify_controller = [0_u32; 16];
    identify_controller[0] = u32::from(ADMIN_IDENTIFY);
    set_prp(&mut identify_controller, data_physical);
    identify_controller[10] = 1;
    clear_page(data_virtual);
    admin.submit(registers, doorbell_stride, identify_controller)?;
    // SAFETY: Identify Controller completed into the exclusive data page.
    let controller = unsafe { core::slice::from_raw_parts(data_virtual as *const u8, PAGE_BYTES) };
    let namespace_count = le_u32(controller, 516);
    let mut model = [b' '; 40];
    model.copy_from_slice(&controller[24..64]);
    let model_length = trimmed_length(&model)?;
    if namespace_count == 0 {
        return Err(StorageError::Device);
    }

    let mut identify_namespace = [0_u32; 16];
    identify_namespace[0] = u32::from(ADMIN_IDENTIFY);
    identify_namespace[1] = 1;
    set_prp(&mut identify_namespace, data_physical);
    clear_page(data_virtual);
    admin.submit(registers, doorbell_stride, identify_namespace)?;
    // SAFETY: Identify Namespace completed into the exclusive data page.
    let namespace = unsafe { core::slice::from_raw_parts(data_virtual as *const u8, PAGE_BYTES) };
    let sector_count = le_u64(namespace, 0);
    let format = usize::from(namespace[26] & 0x0f);
    let format_offset = 128_usize
        .checked_add(format.checked_mul(4).ok_or(StorageError::TooLarge)?)
        .ok_or(StorageError::TooLarge)?;
    if format > usize::from(namespace[25]) || format_offset + 4 > namespace.len() {
        return Err(StorageError::Device);
    }
    let metadata_size =
        u16::from_le_bytes([namespace[format_offset], namespace[format_offset + 1]]);
    let lba_shift = namespace[format_offset + 2];
    let sector_size = 1_u32
        .checked_shl(u32::from(lba_shift))
        .ok_or(StorageError::UnsupportedSectorSize)?;
    if sector_count == 0
        || metadata_size != 0
        || !(512..=4096).contains(&sector_size)
        || !sector_size.is_power_of_two()
    {
        return Err(StorageError::UnsupportedSectorSize);
    }
    Ok(NvmeDevice {
        registers,
        capabilities,
        version: read32(registers + REG_VERSION),
        doorbell_stride,
        admin,
        io,
        data_physical,
        data_virtual,
        namespace_id: 1,
        sector_count,
        sector_size,
        model,
        model_length,
        controller_index,
        recovery_count: 0,
    })
}

fn configure_controller(
    registers: u64,
    capabilities: u64,
    admin: &mut Queue,
    io: &mut Queue,
    doorbell_stride: u64,
) -> Result<(), StorageError> {
    write32(registers + REG_INTERRUPT_MASK_SET, u32::MAX);
    write32(registers + REG_CONTROLLER_CONFIG, 0);
    wait_ready(registers, false, capabilities)?;
    admin.reset();
    io.reset();
    let queue_size = u32::from(admin.depth - 1);
    write32(
        registers + REG_ADMIN_QUEUE_ATTRIBUTES,
        queue_size | (queue_size << 16),
    );
    write64(
        registers + REG_ADMIN_SUBMISSION_QUEUE,
        admin.submission_physical,
    );
    write64(
        registers + REG_ADMIN_COMPLETION_QUEUE,
        admin.completion_physical,
    );
    let configuration = (6 << 16) | (4 << 20) | CC_ENABLE;
    write32(registers + REG_CONTROLLER_CONFIG, configuration);
    wait_ready(registers, true, capabilities)?;

    let mut create_cq = [0_u32; 16];
    create_cq[0] = u32::from(ADMIN_CREATE_IO_CQ);
    set_prp(&mut create_cq, io.completion_physical);
    create_cq[10] = u32::from(io.id) | (u32::from(io.depth - 1) << 16);
    create_cq[11] = 1;
    admin.submit(registers, doorbell_stride, create_cq)?;
    let mut create_sq = [0_u32; 16];
    create_sq[0] = u32::from(ADMIN_CREATE_IO_SQ);
    set_prp(&mut create_sq, io.submission_physical);
    create_sq[10] = u32::from(io.id) | (u32::from(io.depth - 1) << 16);
    create_sq[11] = 1 | (u32::from(io.id) << 16);
    admin.submit(registers, doorbell_stride, create_sq)
}

fn wait_ready(registers: u64, ready: bool, capabilities: u64) -> Result<(), StorageError> {
    let timeout_units = ((capabilities >> 24) & 0xff).max(1);
    let polls = usize::try_from(timeout_units)
        .unwrap_or(1)
        .saturating_mul(POLL_LIMIT / 4);
    for _ in 0..polls {
        if (read32(registers + REG_CONTROLLER_STATUS) & CSTS_READY != 0) == ready {
            return Ok(());
        }
        spin_loop();
    }
    Err(StorageError::Timeout)
}

fn set_prp(command: &mut [u32; 16], physical: u64) {
    let bytes = physical.to_le_bytes();
    command[6] = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    command[7] = u32::from_le_bytes(bytes[4..].try_into().unwrap());
}

fn clear_page(virtual_address: u64) {
    // SAFETY: Callers provide the exclusive controller data page.
    unsafe { core::ptr::write_bytes(virtual_address as *mut u8, 0, PAGE_BYTES) };
}

fn trimmed_length(bytes: &[u8]) -> Result<u8, StorageError> {
    u8::try_from(
        bytes
            .iter()
            .rposition(|byte| *byte != b' ' && *byte != 0)
            .map_or(0, |index| index + 1),
    )
    .map_err(|_| StorageError::TooLarge)
}

fn doorbell(registers: u64, queue_id: u16, completion: bool, stride: u64) -> u64 {
    registers + REG_DOORBELLS + (u64::from(queue_id) * 2 + u64::from(completion)) * stride
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
    let pages = offset
        .checked_add(length)
        .ok_or(PageMapError::AddressOverflow)?
        .div_ceil(PAGE_SIZE);
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

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read32(address: u64) -> u32 {
    // SAFETY: Callers pass aligned registers within the mapped NVMe BAR.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write32(address: u64, value: u32) {
    // SAFETY: Callers pass aligned writable registers within the mapped BAR.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn read64(address: u64) -> u64 {
    // SAFETY: CAP is an aligned 64-bit register in the mapped NVMe BAR.
    unsafe { core::ptr::read_volatile(address as *const u64) }
}

fn write64(address: u64, value: u64) {
    // SAFETY: ASQ/ACQ are aligned 64-bit registers in the mapped NVMe BAR.
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
}

fn read_memory32(address: u64) -> u32 {
    // SAFETY: Queue entries are aligned, exclusive DMA memory.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write_memory32(address: u64, value: u32) {
    // SAFETY: Queue entries are aligned, exclusive DMA memory.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}
