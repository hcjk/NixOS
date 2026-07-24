use nexos_storage::{BlockDevice, MbrPartition, StorageError, parse_gpt_header, parse_mbr};

use crate::ahci::{self, AhciDevice, AhciProbeStats};
use crate::ide::{self, IdeDevice};
use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;
use crate::pci::PciInventory;

const MAX_STORAGE_DEVICES: usize = 8;

#[derive(Clone, Copy)]
pub enum StorageDevice {
    Ahci(AhciDevice),
    Ide(IdeDevice),
}

impl StorageDevice {
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Ahci(_) => "sata-ahci",
            Self::Ide(_) => "ide-pio",
        }
    }

    #[must_use]
    pub fn model_bytes(&self) -> &[u8] {
        match self {
            Self::Ahci(device) => device.model_bytes(),
            Self::Ide(device) => device.model_bytes(),
        }
    }

    #[must_use]
    pub const fn location(&self) -> DeviceLocation {
        match self {
            Self::Ahci(device) => DeviceLocation::AhciPort {
                port: device.port_number(),
                version: device.controller_version(),
            },
            Self::Ide(device) => DeviceLocation::IdePosition {
                slave: device.is_slave(),
            },
        }
    }
}

impl BlockDevice for StorageDevice {
    fn sector_size(&self) -> u32 {
        match self {
            Self::Ahci(device) => device.sector_size(),
            Self::Ide(device) => device.sector_size(),
        }
    }

    fn sector_count(&self) -> u64 {
        match self {
            Self::Ahci(device) => device.sector_count(),
            Self::Ide(device) => device.sector_count(),
        }
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        match self {
            Self::Ahci(device) => device.read_sectors(lba, output),
            Self::Ide(device) => device.read_sectors(lba, output),
        }
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        match self {
            Self::Ahci(device) => device.write_sectors(lba, input),
            Self::Ide(device) => device.write_sectors(lba, input),
        }
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        match self {
            Self::Ahci(device) => device.flush(),
            Self::Ide(device) => device.flush(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum DeviceLocation {
    AhciPort { port: u8, version: u32 },
    IdePosition { slave: bool },
}

#[derive(Clone, Copy)]
pub enum PartitionProbe {
    None,
    Mbr {
        entries: [Option<MbrPartition>; 4],
    },
    Gpt {
        entry_count: u32,
        first_usable_lba: u64,
        last_usable_lba: u64,
    },
    Invalid,
    ReadError(StorageError),
}

pub struct StorageManager {
    devices: [Option<StorageDevice>; MAX_STORAGE_DEVICES],
    count: usize,
    ahci_count: usize,
    ide_count: usize,
    ahci_probe: AhciProbeStats,
}

impl StorageManager {
    #[must_use]
    pub fn discover(
        inventory: &PciInventory,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Self {
        let mut manager = Self {
            devices: [None; MAX_STORAGE_DEVICES],
            count: 0,
            ahci_count: 0,
            ide_count: 0,
            ahci_probe: AhciProbeStats::default(),
        };

        let mut ahci_devices = [None; MAX_STORAGE_DEVICES];
        let ahci_count = ahci::discover(
            inventory,
            paging,
            allocator,
            &mut ahci_devices,
            &mut manager.ahci_probe,
        );
        for device in ahci_devices.into_iter().take(ahci_count).flatten() {
            manager.push(StorageDevice::Ahci(device));
            manager.ahci_count += 1;
        }

        let mut ide_devices = [None; MAX_STORAGE_DEVICES];
        let ide_count = ide::discover(inventory, &mut ide_devices);
        for device in ide_devices
            .into_iter()
            .take(ide_count)
            .flatten()
            .take(MAX_STORAGE_DEVICES - manager.count)
        {
            manager.push(StorageDevice::Ide(device));
            manager.ide_count += 1;
        }
        manager
    }

    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    #[must_use]
    pub const fn ahci_count(&self) -> usize {
        self.ahci_count
    }

    #[must_use]
    pub const fn ide_count(&self) -> usize {
        self.ide_count
    }

    #[must_use]
    pub const fn ahci_probe(&self) -> AhciProbeStats {
        self.ahci_probe
    }

    #[must_use]
    pub fn device(&self, index: usize) -> Option<&StorageDevice> {
        self.devices.get(index)?.as_ref()
    }

    pub fn device_mut(&mut self, index: usize) -> Option<&mut StorageDevice> {
        self.devices.get_mut(index)?.as_mut()
    }

    pub fn probe_partitions(&mut self, index: usize) -> PartitionProbe {
        let Some(device) = self.device_mut(index) else {
            return PartitionProbe::Invalid;
        };
        if device.sector_size() != 512 {
            return PartitionProbe::Invalid;
        }
        let mut sector = [0_u8; 512];
        if let Err(error) = device.read_sectors(0, &mut sector) {
            return PartitionProbe::ReadError(error);
        }
        let Ok(entries) = parse_mbr(&sector) else {
            return PartitionProbe::None;
        };
        if entries
            .iter()
            .flatten()
            .any(|partition| partition.partition_type == 0xee)
        {
            if let Err(error) = device.read_sectors(1, &mut sector) {
                return PartitionProbe::ReadError(error);
            }
            return parse_gpt_header(&sector).map_or(PartitionProbe::Invalid, |header| {
                PartitionProbe::Gpt {
                    entry_count: header.entry_count,
                    first_usable_lba: header.first_usable_lba,
                    last_usable_lba: header.last_usable_lba,
                }
            });
        }
        PartitionProbe::Mbr { entries }
    }

    fn push(&mut self, device: StorageDevice) {
        if self.count < self.devices.len() {
            self.devices[self.count] = Some(device);
            self.count += 1;
        }
    }
}
