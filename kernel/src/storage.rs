use alloc::boxed::Box;
use alloc::vec;

use nexos_storage::{BlockDevice, MbrPartition, StorageError, parse_gpt_header, parse_mbr};

use crate::ahci::{self, AhciDevice, AhciProbeStats};
use crate::ide::{self, IdeDevice};
use crate::memory::FrameAllocator;
use crate::nvme::{self, NvmeDevice, NvmeProbeStats};
use crate::paging::PagingInfo;
use crate::pci::PciInventory;
use crate::usb::UsbManager;
use crate::xhci::{MAX_USB_STORAGE_DEVICES, UsbMassStorageDevice};

const MAX_STORAGE_DEVICES: usize = 16;

#[derive(Clone, Copy)]
pub enum StorageDevice {
    Nvme(NvmeDevice),
    Ahci(AhciDevice),
    Ide(IdeDevice),
    Usb(UsbMassStorageDevice),
}

impl StorageDevice {
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Nvme(_) => "nvme",
            Self::Ahci(_) => "sata-ahci",
            Self::Ide(_) => "ide-pio",
            Self::Usb(_) => "usb-bot-scsi",
        }
    }

    #[must_use]
    pub fn model_bytes(&self) -> &[u8] {
        match self {
            Self::Nvme(device) => device.model_bytes(),
            Self::Ahci(device) => device.model_bytes(),
            Self::Ide(device) => device.model_bytes(),
            Self::Usb(device) => device.model_bytes(),
        }
    }

    #[must_use]
    pub const fn location(&self) -> DeviceLocation {
        match self {
            Self::Nvme(device) => DeviceLocation::NvmeNamespace {
                controller: device.controller_index(),
                namespace: device.namespace_id(),
                version: device.controller_version(),
                recoveries: device.recovery_count(),
            },
            Self::Ahci(device) => DeviceLocation::AhciPort {
                port: device.port_number(),
                version: device.controller_version(),
            },
            Self::Ide(device) => DeviceLocation::IdePosition {
                slave: device.is_slave(),
            },
            Self::Usb(device) => DeviceLocation::UsbPosition {
                controller: device.controller_index(),
                slot: device.slot_id(),
                root_port: device.root_port(),
            },
        }
    }
}

impl BlockDevice for StorageDevice {
    fn sector_size(&self) -> u32 {
        match self {
            Self::Nvme(device) => device.sector_size(),
            Self::Ahci(device) => device.sector_size(),
            Self::Ide(device) => device.sector_size(),
            Self::Usb(device) => device.sector_size(),
        }
    }

    fn sector_count(&self) -> u64 {
        match self {
            Self::Nvme(device) => device.sector_count(),
            Self::Ahci(device) => device.sector_count(),
            Self::Ide(device) => device.sector_count(),
            Self::Usb(device) => device.sector_count(),
        }
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        match self {
            Self::Nvme(device) => device.read_sectors(lba, output),
            Self::Ahci(device) => device.read_sectors(lba, output),
            Self::Ide(device) => device.read_sectors(lba, output),
            Self::Usb(device) => device.read_sectors(lba, output),
        }
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        match self {
            Self::Nvme(device) => device.write_sectors(lba, input),
            Self::Ahci(device) => device.write_sectors(lba, input),
            Self::Ide(device) => device.write_sectors(lba, input),
            Self::Usb(device) => device.write_sectors(lba, input),
        }
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        match self {
            Self::Nvme(device) => device.flush(),
            Self::Ahci(device) => device.flush(),
            Self::Ide(device) => device.flush(),
            Self::Usb(device) => device.flush(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum DeviceLocation {
    NvmeNamespace {
        controller: u8,
        namespace: u32,
        version: u32,
        recoveries: u32,
    },
    AhciPort {
        port: u8,
        version: u32,
    },
    IdePosition {
        slave: bool,
    },
    UsbPosition {
        controller: u8,
        slot: u8,
        root_port: u8,
    },
}

#[derive(Clone, Copy)]
pub struct DeviceNode {
    pub prefix: &'static str,
    pub ordinal: usize,
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
    devices: Box<[Option<StorageDevice>]>,
    count: usize,
    ahci_count: usize,
    nvme_count: usize,
    ide_count: usize,
    usb_count: usize,
    ahci_probe: AhciProbeStats,
    nvme_probe: NvmeProbeStats,
}

impl StorageManager {
    #[must_use]
    pub fn discover(
        inventory: &PciInventory,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
        usb: &mut UsbManager,
    ) -> Self {
        let mut manager = Self {
            devices: vec![None; MAX_STORAGE_DEVICES].into_boxed_slice(),
            count: 0,
            ahci_count: 0,
            nvme_count: 0,
            ide_count: 0,
            usb_count: 0,
            ahci_probe: AhciProbeStats::default(),
            nvme_probe: NvmeProbeStats::default(),
        };

        let mut nvme_devices = vec![None; MAX_STORAGE_DEVICES].into_boxed_slice();
        let nvme_count = nvme::discover(
            inventory,
            paging,
            allocator,
            &mut nvme_devices,
            &mut manager.nvme_probe,
        );
        for device in nvme_devices.iter().copied().take(nvme_count).flatten() {
            manager.push(StorageDevice::Nvme(device));
            manager.nvme_count += 1;
        }

        let mut ahci_devices = vec![None; MAX_STORAGE_DEVICES].into_boxed_slice();
        let ahci_count = ahci::discover(
            inventory,
            paging,
            allocator,
            &mut ahci_devices,
            &mut manager.ahci_probe,
        );
        for device in ahci_devices.iter().copied().take(ahci_count).flatten() {
            manager.push(StorageDevice::Ahci(device));
            manager.ahci_count += 1;
        }

        let mut ide_devices = vec![None; MAX_STORAGE_DEVICES].into_boxed_slice();
        let ide_count = ide::discover(inventory, &mut ide_devices);
        for device in ide_devices
            .iter()
            .copied()
            .take(ide_count)
            .flatten()
            .take(MAX_STORAGE_DEVICES - manager.count)
        {
            manager.push(StorageDevice::Ide(device));
            manager.ide_count += 1;
        }

        let mut usb_devices = vec![None; MAX_USB_STORAGE_DEVICES].into_boxed_slice();
        let usb_count = usb.mass_storage_devices(&mut usb_devices);
        for device in usb_devices
            .iter()
            .copied()
            .take(usb_count)
            .flatten()
            .take(MAX_STORAGE_DEVICES - manager.count)
        {
            manager.push(StorageDevice::Usb(device));
            manager.usb_count += 1;
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
    pub const fn nvme_count(&self) -> usize {
        self.nvme_count
    }

    #[must_use]
    pub const fn ide_count(&self) -> usize {
        self.ide_count
    }

    #[must_use]
    pub const fn usb_count(&self) -> usize {
        self.usb_count
    }

    #[must_use]
    pub const fn ahci_probe(&self) -> AhciProbeStats {
        self.ahci_probe
    }

    #[must_use]
    pub const fn nvme_probe(&self) -> NvmeProbeStats {
        self.nvme_probe
    }

    pub fn recover(&mut self, index: usize) -> Result<(), StorageError> {
        match self.device_mut(index).ok_or(StorageError::NotFound)? {
            StorageDevice::Nvme(device) => device.recover(),
            _ => Err(StorageError::UnsupportedFilesystem),
        }
    }

    #[must_use]
    pub fn device(&self, index: usize) -> Option<&StorageDevice> {
        self.devices.get(index)?.as_ref()
    }

    pub fn device_mut(&mut self, index: usize) -> Option<&mut StorageDevice> {
        self.devices.get_mut(index)?.as_mut()
    }

    #[must_use]
    pub fn has_sector_size(&self, sector_size: u32) -> bool {
        self.devices[..self.count]
            .iter()
            .flatten()
            .any(|device| device.sector_size() == sector_size)
    }

    #[must_use]
    pub fn device_node(&self, index: usize) -> Option<DeviceNode> {
        let device = self.device(index)?;
        let prefix = match device {
            StorageDevice::Nvme(_) => "nvme",
            StorageDevice::Ahci(_) => "sata",
            StorageDevice::Ide(_) => "ide",
            StorageDevice::Usb(_) => "usb",
        };
        let ordinal = self.devices[..index]
            .iter()
            .flatten()
            .filter(|candidate| {
                matches!(
                    (device, candidate),
                    (StorageDevice::Nvme(_), StorageDevice::Nvme(_))
                        | (StorageDevice::Ahci(_), StorageDevice::Ahci(_))
                        | (StorageDevice::Ide(_), StorageDevice::Ide(_))
                        | (StorageDevice::Usb(_), StorageDevice::Usb(_))
                )
            })
            .count();
        Some(DeviceNode { prefix, ordinal })
    }

    pub fn probe_partitions(&mut self, index: usize) -> PartitionProbe {
        let Some(device) = self.device_mut(index) else {
            return PartitionProbe::Invalid;
        };
        let Ok(sector_size) = usize::try_from(device.sector_size()) else {
            return PartitionProbe::Invalid;
        };
        if !(512..=4096).contains(&sector_size) || !sector_size.is_power_of_two() {
            return PartitionProbe::Invalid;
        }
        let mut sector = alloc::vec![0_u8; sector_size];
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
