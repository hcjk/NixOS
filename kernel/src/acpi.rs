use core::fmt;

const SDT_HEADER_SIZE: usize = 36;
const MAX_TABLE_SIZE: usize = 1024 * 1024;
const MAX_INTERRUPT_OVERRIDES: usize = 16;

#[derive(Clone, Copy)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u64,
    pub global_interrupt_base: u32,
}

#[derive(Clone, Copy)]
pub struct InterruptOverride {
    pub source_irq: u8,
    pub global_interrupt: u32,
    pub flags: u16,
}

impl InterruptOverride {
    const EMPTY: Self = Self {
        source_irq: 0,
        global_interrupt: 0,
        flags: 0,
    };
}

#[derive(Clone, Copy)]
pub struct HpetInfo {
    pub address: u64,
    pub address_space: u8,
    pub minimum_tick: u16,
}

#[derive(Clone, Copy)]
pub struct McfgInfo {
    pub base_address: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

pub struct PlatformInfo {
    pub revision: u8,
    pub uses_xsdt: bool,
    pub table_count: usize,
    pub local_apic_address: Option<u64>,
    pub enabled_processor_count: usize,
    pub io_apic: Option<IoApicInfo>,
    pub hpet: Option<HpetInfo>,
    pub mcfg: Option<McfgInfo>,
    interrupt_overrides: [InterruptOverride; MAX_INTERRUPT_OVERRIDES],
    interrupt_override_count: usize,
}

impl PlatformInfo {
    pub fn discover(rsdp_address: usize, hhdm_offset: u64) -> Result<Self, AcpiError> {
        let rsdp_virtual = rsdp_address as u64;
        if read_signature8(rsdp_virtual) != *b"RSD PTR " {
            return Err(AcpiError::BadRsdpSignature);
        }
        if !checksum_is_valid(rsdp_virtual, 20) {
            return Err(AcpiError::BadRsdpChecksum);
        }

        let revision = read_u8(rsdp_virtual + 15);
        let legacy_root_address = u64::from(read_u32(rsdp_virtual + 16));
        let (root_physical, uses_xsdt) = if revision >= 2 {
            let length = usize::try_from(read_u32(rsdp_virtual + 20))
                .map_err(|_| AcpiError::InvalidLength)?;
            if !(36..=4096).contains(&length) || !checksum_is_valid(rsdp_virtual, length) {
                return Err(AcpiError::BadExtendedRsdpChecksum);
            }
            let xsdt_address = read_u64(rsdp_virtual + 24);
            if xsdt_address != 0 {
                (xsdt_address, true)
            } else {
                (legacy_root_address, false)
            }
        } else {
            (legacy_root_address, false)
        };
        let root = physical_to_virtual(hhdm_offset, root_physical)?;
        let expected_signature = if uses_xsdt { *b"XSDT" } else { *b"RSDT" };
        validate_sdt(root, expected_signature)?;
        let root_length =
            usize::try_from(read_u32(root + 4)).map_err(|_| AcpiError::InvalidLength)?;
        let entry_size = if uses_xsdt { 8 } else { 4 };
        let entry_bytes = root_length
            .checked_sub(SDT_HEADER_SIZE)
            .ok_or(AcpiError::InvalidLength)?;
        if entry_bytes % entry_size != 0 {
            return Err(AcpiError::InvalidLength);
        }

        let mut platform = Self {
            revision,
            uses_xsdt,
            table_count: entry_bytes / entry_size,
            local_apic_address: None,
            enabled_processor_count: 0,
            io_apic: None,
            hpet: None,
            mcfg: None,
            interrupt_overrides: [InterruptOverride::EMPTY; MAX_INTERRUPT_OVERRIDES],
            interrupt_override_count: 0,
        };

        for index in 0..platform.table_count {
            let entry = root + SDT_HEADER_SIZE as u64 + (index * entry_size) as u64;
            let physical = if uses_xsdt {
                read_u64(entry)
            } else {
                u64::from(read_u32(entry))
            };
            let table = physical_to_virtual(hhdm_offset, physical)?;
            let signature = read_signature4(table);
            if validate_sdt(table, signature).is_err() {
                continue;
            }
            match &signature {
                b"APIC" => platform.parse_madt(table)?,
                b"HPET" => platform.parse_hpet(table)?,
                b"MCFG" => platform.parse_mcfg(table)?,
                _ => {}
            }
        }
        Ok(platform)
    }

    #[must_use]
    pub fn global_interrupt_for(&self, irq: u8) -> (u32, u16) {
        self.interrupt_overrides[..self.interrupt_override_count]
            .iter()
            .find(|entry| entry.source_irq == irq)
            .map_or((u32::from(irq), 0), |entry| {
                (entry.global_interrupt, entry.flags)
            })
    }

    #[must_use]
    pub const fn interrupt_override_count(&self) -> usize {
        self.interrupt_override_count
    }

    fn parse_madt(&mut self, table: u64) -> Result<(), AcpiError> {
        let length = table_length(table)?;
        if length < 44 {
            return Err(AcpiError::InvalidLength);
        }
        self.local_apic_address = Some(u64::from(read_u32(table + 36)));
        let mut offset = 44_usize;
        while offset + 2 <= length {
            let entry = table + offset as u64;
            let entry_type = read_u8(entry);
            let entry_length = usize::from(read_u8(entry + 1));
            if entry_length < 2 || offset + entry_length > length {
                return Err(AcpiError::InvalidMadtEntry);
            }
            match entry_type {
                0 if entry_length >= 8 => {
                    let flags = read_u32(entry + 4);
                    if flags & 3 != 0 {
                        self.enabled_processor_count += 1;
                    }
                }
                1 if entry_length >= 12 && self.io_apic.is_none() => {
                    self.io_apic = Some(IoApicInfo {
                        id: read_u8(entry + 2),
                        address: u64::from(read_u32(entry + 4)),
                        global_interrupt_base: read_u32(entry + 8),
                    });
                }
                2 if entry_length >= 10
                    && self.interrupt_override_count < MAX_INTERRUPT_OVERRIDES =>
                {
                    self.interrupt_overrides[self.interrupt_override_count] = InterruptOverride {
                        source_irq: read_u8(entry + 3),
                        global_interrupt: read_u32(entry + 4),
                        flags: read_u16(entry + 8),
                    };
                    self.interrupt_override_count += 1;
                }
                5 if entry_length >= 12 => {
                    self.local_apic_address = Some(read_u64(entry + 4));
                }
                _ => {}
            }
            offset += entry_length;
        }
        Ok(())
    }

    fn parse_hpet(&mut self, table: u64) -> Result<(), AcpiError> {
        if table_length(table)? < 56 {
            return Err(AcpiError::InvalidLength);
        }
        self.hpet = Some(HpetInfo {
            address_space: read_u8(table + 40),
            address: read_u64(table + 44),
            minimum_tick: read_u16(table + 53),
        });
        Ok(())
    }

    fn parse_mcfg(&mut self, table: u64) -> Result<(), AcpiError> {
        if table_length(table)? < 60 {
            return Err(AcpiError::InvalidLength);
        }
        self.mcfg = Some(McfgInfo {
            base_address: read_u64(table + 44),
            segment_group: read_u16(table + 52),
            start_bus: read_u8(table + 54),
            end_bus: read_u8(table + 55),
        });
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AcpiError {
    BadRsdpSignature,
    BadRsdpChecksum,
    BadExtendedRsdpChecksum,
    BadTableSignature,
    BadTableChecksum,
    InvalidLength,
    InvalidMadtEntry,
    AddressOverflow,
}

impl fmt::Display for AcpiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

fn validate_sdt(address: u64, expected_signature: [u8; 4]) -> Result<(), AcpiError> {
    if read_signature4(address) != expected_signature {
        return Err(AcpiError::BadTableSignature);
    }
    let length = table_length(address)?;
    if !checksum_is_valid(address, length) {
        return Err(AcpiError::BadTableChecksum);
    }
    Ok(())
}

fn table_length(address: u64) -> Result<usize, AcpiError> {
    let length = usize::try_from(read_u32(address + 4)).map_err(|_| AcpiError::InvalidLength)?;
    if !(SDT_HEADER_SIZE..=MAX_TABLE_SIZE).contains(&length) {
        return Err(AcpiError::InvalidLength);
    }
    Ok(length)
}

fn physical_to_virtual(hhdm_offset: u64, physical: u64) -> Result<u64, AcpiError> {
    hhdm_offset
        .checked_add(physical)
        .ok_or(AcpiError::AddressOverflow)
}

fn checksum_is_valid(address: u64, length: usize) -> bool {
    let mut checksum = 0_u8;
    for offset in 0..length {
        checksum = checksum.wrapping_add(read_u8(address + offset as u64));
    }
    checksum == 0
}

fn read_signature4(address: u64) -> [u8; 4] {
    [
        read_u8(address),
        read_u8(address + 1),
        read_u8(address + 2),
        read_u8(address + 3),
    ]
}

fn read_signature8(address: u64) -> [u8; 8] {
    [
        read_u8(address),
        read_u8(address + 1),
        read_u8(address + 2),
        read_u8(address + 3),
        read_u8(address + 4),
        read_u8(address + 5),
        read_u8(address + 6),
        read_u8(address + 7),
    ]
}

fn read_u8(address: u64) -> u8 {
    // SAFETY: Callers only pass bootloader-provided or checksum-validated ACPI
    // addresses, and byte reads do not impose alignment requirements.
    unsafe { core::ptr::read_volatile(address as *const u8) }
}

fn read_u16(address: u64) -> u16 {
    u16::from_le_bytes([read_u8(address), read_u8(address + 1)])
}

fn read_u32(address: u64) -> u32 {
    u32::from_le_bytes([
        read_u8(address),
        read_u8(address + 1),
        read_u8(address + 2),
        read_u8(address + 3),
    ])
}

fn read_u64(address: u64) -> u64 {
    u64::from_le_bytes([
        read_u8(address),
        read_u8(address + 1),
        read_u8(address + 2),
        read_u8(address + 3),
        read_u8(address + 4),
        read_u8(address + 5),
        read_u8(address + 6),
        read_u8(address + 7),
    ])
}
