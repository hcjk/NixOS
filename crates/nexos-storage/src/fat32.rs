use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::{BlockDevice, StorageError};

const FAT32_MIN_CLUSTERS: u32 = 65_525;
const FAT_ENTRY_MASK: u32 = 0x0fff_ffff;
const FAT_EOC: u32 = 0x0fff_ffff;
const FAT_EOC_MIN: u32 = 0x0fff_fff8;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = 0x0f;
const ATTR_VOLUME_ID: u8 = 0x08;
const FAT_COUNT: u8 = 2;
const RESERVED_SECTORS: u16 = 32;
const ROOT_CLUSTER: u32 = 2;
const FORMAT_CHUNK_SECTORS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fat32Info {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub total_sectors: u32,
    pub sectors_per_fat: u32,
    pub root_cluster: u32,
    pub total_clusters: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FatDirectoryEntry {
    pub name: String,
    pub is_directory: bool,
    pub size: u32,
    pub first_cluster: u32,
}

#[derive(Clone, Copy)]
struct EntryLocation {
    sector: u64,
    offset: usize,
}

#[derive(Clone, Copy)]
struct RawDirectoryEntry {
    location: EntryLocation,
    bytes: [u8; 32],
}

pub struct Fat32<D> {
    device: D,
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    fat_count: u8,
    sectors_per_fat: u32,
    total_sectors: u32,
    root_cluster: u32,
    first_data_sector: u32,
    total_clusters: u32,
}

pub fn format_fat32<D: BlockDevice>(
    device: &mut D,
    volume_id: u32,
) -> Result<Fat32Info, StorageError> {
    let bytes_per_sector =
        usize::try_from(device.sector_size()).map_err(|_| StorageError::TooLarge)?;
    if bytes_per_sector != 512 {
        return Err(StorageError::UnsupportedSectorSize);
    }
    let total_sectors = u32::try_from(device.sector_count()).map_err(|_| StorageError::TooLarge)?;
    let (sectors_per_cluster, sectors_per_fat, total_clusters) =
        choose_format_geometry(total_sectors, bytes_per_sector)?;

    let mut sector = vec![0_u8; bytes_per_sector];
    sector[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    sector[3..11].copy_from_slice(b"NEXOS   ");
    let bytes_per_sector_u16 =
        u16::try_from(bytes_per_sector).map_err(|_| StorageError::TooLarge)?;
    sector[11..13].copy_from_slice(&bytes_per_sector_u16.to_le_bytes());
    sector[13] = sectors_per_cluster;
    sector[14..16].copy_from_slice(&RESERVED_SECTORS.to_le_bytes());
    sector[16] = FAT_COUNT;
    sector[21] = 0xf8;
    sector[24..26].copy_from_slice(&63_u16.to_le_bytes());
    sector[26..28].copy_from_slice(&255_u16.to_le_bytes());
    sector[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    sector[36..40].copy_from_slice(&sectors_per_fat.to_le_bytes());
    sector[44..48].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
    sector[48..50].copy_from_slice(&1_u16.to_le_bytes());
    sector[50..52].copy_from_slice(&6_u16.to_le_bytes());
    sector[64] = 0x80;
    sector[66] = 0x29;
    sector[67..71].copy_from_slice(&volume_id.to_le_bytes());
    sector[71..82].copy_from_slice(b"NEXOS BOOT ");
    sector[82..90].copy_from_slice(b"FAT32   ");
    sector[510..512].copy_from_slice(&crate::MBR_SIGNATURE);
    device.write_sectors(0, &sector)?;
    device.write_sectors(6, &sector)?;

    sector.fill(0);
    sector[0..4].copy_from_slice(&0x4161_5252_u32.to_le_bytes());
    sector[484..488].copy_from_slice(&0x6141_7272_u32.to_le_bytes());
    sector[488..492].copy_from_slice(&(total_clusters - 1).to_le_bytes());
    sector[492..496].copy_from_slice(&3_u32.to_le_bytes());
    sector[508..512].copy_from_slice(&0xaa55_0000_u32.to_le_bytes());
    device.write_sectors(1, &sector)?;
    device.write_sectors(7, &sector)?;

    let zero_bytes = bytes_per_sector
        .checked_mul(FORMAT_CHUNK_SECTORS)
        .ok_or(StorageError::TooLarge)?;
    let zeros = vec![0_u8; zero_bytes];
    zero_range(
        device,
        u64::from(RESERVED_SECTORS),
        u64::from(FAT_COUNT) * u64::from(sectors_per_fat) + u64::from(sectors_per_cluster),
        &zeros,
    )?;

    sector.fill(0);
    sector[0..4].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
    sector[4..8].copy_from_slice(&0xffff_ffff_u32.to_le_bytes());
    sector[8..12].copy_from_slice(&FAT_EOC.to_le_bytes());
    for fat in 0..FAT_COUNT {
        let first_fat_sector =
            u64::from(RESERVED_SECTORS) + u64::from(fat) * u64::from(sectors_per_fat);
        device.write_sectors(first_fat_sector, &sector)?;
    }
    device.flush()?;
    Ok(Fat32Info {
        bytes_per_sector: 512,
        sectors_per_cluster,
        total_sectors,
        sectors_per_fat,
        root_cluster: ROOT_CLUSTER,
        total_clusters,
    })
}

fn choose_format_geometry(
    total_sectors: u32,
    bytes_per_sector: usize,
) -> Result<(u8, u32, u32), StorageError> {
    for sectors_per_cluster in [1_u8, 2, 4, 8, 16, 32, 64, 128] {
        let possible_data = total_sectors
            .checked_sub(u32::from(RESERVED_SECTORS))
            .ok_or(StorageError::UnsupportedFilesystem)?;
        let maximum_clusters = possible_data / u32::from(sectors_per_cluster);
        let fat_bytes = u64::from(maximum_clusters + 2) * 4;
        let sectors_per_fat = u32::try_from(
            fat_bytes
                .div_ceil(u64::try_from(bytes_per_sector).map_err(|_| StorageError::TooLarge)?),
        )
        .map_err(|_| StorageError::TooLarge)?;
        let overhead = u32::from(RESERVED_SECTORS)
            .checked_add(u32::from(FAT_COUNT) * sectors_per_fat)
            .ok_or(StorageError::TooLarge)?;
        let data_sectors = total_sectors
            .checked_sub(overhead)
            .ok_or(StorageError::UnsupportedFilesystem)?;
        let clusters = data_sectors / u32::from(sectors_per_cluster);
        let fat_capacity = u64::from(sectors_per_fat)
            * u64::try_from(bytes_per_sector).map_err(|_| StorageError::TooLarge)?
            / 4;
        if (FAT32_MIN_CLUSTERS..=0x0fff_fff5).contains(&clusters)
            && fat_capacity >= u64::from(clusters) + 2
        {
            return Ok((sectors_per_cluster, sectors_per_fat, clusters));
        }
    }
    Err(StorageError::UnsupportedFilesystem)
}

fn zero_range<D: BlockDevice>(
    device: &mut D,
    first_lba: u64,
    sector_count: u64,
    zeros: &[u8],
) -> Result<(), StorageError> {
    let sectors_per_chunk = u64::try_from(zeros.len()).map_err(|_| StorageError::TooLarge)?
        / u64::from(device.sector_size());
    let mut completed = 0_u64;
    while completed < sector_count {
        let sectors = (sector_count - completed).min(sectors_per_chunk);
        let bytes = usize::try_from(sectors)
            .ok()
            .and_then(|count| count.checked_mul(device.sector_size() as usize))
            .ok_or(StorageError::TooLarge)?;
        device.write_sectors(first_lba + completed, &zeros[..bytes])?;
        completed += sectors;
    }
    Ok(())
}

impl<D: BlockDevice> Fat32<D> {
    pub fn mount(mut device: D) -> Result<Self, StorageError> {
        let sector_size =
            usize::try_from(device.sector_size()).map_err(|_| StorageError::TooLarge)?;
        if !(512..=4096).contains(&sector_size) {
            return Err(StorageError::UnsupportedSectorSize);
        }
        let mut boot = vec![0; sector_size];
        device.read_sectors(0, &mut boot)?;
        if boot.len() < 512 || boot[510..512] != [0x55, 0xaa] {
            return Err(StorageError::UnsupportedFilesystem);
        }

        let bytes_per_sector = le_u16(&boot, 11);
        let sectors_per_cluster = boot[13];
        let reserved_sectors = le_u16(&boot, 14);
        let fat_count = boot[16];
        let root_entry_count = le_u16(&boot, 17);
        let total_sectors_16 = le_u16(&boot, 19);
        let sectors_per_fat_16 = le_u16(&boot, 22);
        let total_sectors_32 = le_u32(&boot, 32);
        let sectors_per_fat = le_u32(&boot, 36);
        let version = le_u16(&boot, 42);
        let root_cluster = le_u32(&boot, 44);
        let total_sectors = if total_sectors_16 == 0 {
            total_sectors_32
        } else {
            u32::from(total_sectors_16)
        };

        if usize::from(bytes_per_sector) != sector_size
            || !bytes_per_sector.is_power_of_two()
            || sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster > 128
            || reserved_sectors == 0
            || !(1..=2).contains(&fat_count)
            || root_entry_count != 0
            || sectors_per_fat_16 != 0
            || sectors_per_fat == 0
            || total_sectors == 0
            || version != 0
            || root_cluster < 2
            || u64::from(total_sectors) > device.sector_count()
        {
            return Err(StorageError::UnsupportedFilesystem);
        }

        let fat_sectors = u32::from(fat_count)
            .checked_mul(sectors_per_fat)
            .ok_or(StorageError::CorruptFilesystem)?;
        let first_data_sector = u32::from(reserved_sectors)
            .checked_add(fat_sectors)
            .ok_or(StorageError::CorruptFilesystem)?;
        let data_sectors = total_sectors
            .checked_sub(first_data_sector)
            .ok_or(StorageError::CorruptFilesystem)?;
        let total_clusters = data_sectors / u32::from(sectors_per_cluster);
        let fat_capacity = u64::from(sectors_per_fat)
            .checked_mul(u64::from(bytes_per_sector))
            .ok_or(StorageError::CorruptFilesystem)?
            / 4;
        if total_clusters < FAT32_MIN_CLUSTERS
            || fat_capacity < u64::from(total_clusters) + 2
            || root_cluster >= total_clusters + 2
        {
            return Err(StorageError::UnsupportedFilesystem);
        }

        Ok(Self {
            device,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            sectors_per_fat,
            total_sectors,
            root_cluster,
            first_data_sector,
            total_clusters,
        })
    }

    #[must_use]
    pub const fn info(&self) -> Fat32Info {
        Fat32Info {
            bytes_per_sector: self.bytes_per_sector,
            sectors_per_cluster: self.sectors_per_cluster,
            total_sectors: self.total_sectors,
            sectors_per_fat: self.sectors_per_fat,
            root_cluster: self.root_cluster,
            total_clusters: self.total_clusters,
        }
    }

    pub fn into_inner(mut self) -> Result<D, StorageError> {
        self.device.flush()?;
        Ok(self.device)
    }

    pub fn flush(&mut self) -> Result<(), StorageError> {
        self.device.flush()
    }

    pub fn list_dir(&mut self, path: &str) -> Result<Vec<FatDirectoryEntry>, StorageError> {
        let components = path_components(path)?;
        let cluster = self.resolve_directory(&components)?;
        let mut output = Vec::new();
        for raw in self.read_directory(cluster)? {
            output.push(public_entry(&raw.bytes)?);
        }
        Ok(output)
    }

    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, StorageError> {
        let components = path_components(path)?;
        if components.is_empty() {
            return Err(StorageError::NotFound);
        }
        let (parent, name) = self.resolve_parent(&components)?;
        let entry = self
            .find_entry(parent, &name)?
            .ok_or(StorageError::NotFound)?;
        if entry.bytes[11] & ATTR_DIRECTORY != 0 {
            return Err(StorageError::NotFound);
        }
        let size = usize::try_from(le_u32(&entry.bytes, 28)).map_err(|_| StorageError::TooLarge)?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let first_cluster = entry_cluster(&entry.bytes);
        if !self.valid_cluster(first_cluster) {
            return Err(StorageError::CorruptFilesystem);
        }
        let chain = self.cluster_chain(first_cluster)?;
        let cluster_size = self.cluster_size()?;
        let capacity = chain
            .len()
            .checked_mul(cluster_size)
            .ok_or(StorageError::TooLarge)?;
        if size > capacity {
            return Err(StorageError::CorruptFilesystem);
        }
        let mut output = Vec::with_capacity(capacity);
        for cluster in chain {
            let bytes = self.read_cluster(cluster)?;
            output.extend_from_slice(&bytes);
        }
        output.truncate(size);
        Ok(output)
    }

    pub fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), StorageError> {
        let size = u32::try_from(contents.len()).map_err(|_| StorageError::TooLarge)?;
        let components = path_components(path)?;
        if components.is_empty() {
            return Err(StorageError::InvalidName);
        }
        let (parent, name) = self.resolve_parent(&components)?;
        let existing = self.find_entry(parent, &name)?;
        if existing
            .as_ref()
            .is_some_and(|entry| entry.bytes[11] & ATTR_DIRECTORY != 0)
        {
            return Err(StorageError::AlreadyExists);
        }
        let old_cluster = existing
            .as_ref()
            .map_or(0, |entry| entry_cluster(&entry.bytes));

        let cluster_size = self.cluster_size()?;
        let needed = contents.len().div_ceil(cluster_size);
        let new_chain = self.allocate_chain(needed)?;
        if let Err(error) = self.write_chain(&new_chain, contents) {
            let _ = self.release_clusters(&new_chain);
            return Err(error);
        }

        let location = if let Some(entry) = existing {
            entry.location
        } else {
            match self.find_free_slot(parent) {
                Ok(location) => location,
                Err(error) => {
                    let _ = self.release_clusters(&new_chain);
                    return Err(error);
                }
            }
        };
        let mut raw = [0_u8; 32];
        raw[0..11].copy_from_slice(&name);
        raw[11] = ATTR_ARCHIVE;
        let first_cluster = new_chain.first().copied().unwrap_or(0);
        set_entry_cluster(&mut raw, first_cluster);
        raw[28..32].copy_from_slice(&size.to_le_bytes());
        if let Err(error) = self.write_directory_entry(location, &raw) {
            let _ = self.release_clusters(&new_chain);
            return Err(error);
        }
        if self.valid_cluster(old_cluster) {
            self.free_chain(old_cluster)?;
        }
        self.device.flush()
    }

    pub fn write_file_lfn(
        &mut self,
        path: &str,
        short_alias: &str,
        contents: &[u8],
    ) -> Result<(), StorageError> {
        let trimmed = path.trim_matches('/');
        let (parent_path, long_name) = trimmed.rsplit_once('/').ok_or(StorageError::InvalidName)?;
        let long_utf16: Vec<u16> = long_name.encode_utf16().collect();
        if long_utf16.is_empty()
            || long_utf16.len() > 13
            || long_name.contains(['/', '\\'])
            || !long_name.is_ascii()
        {
            return Err(StorageError::InvalidName);
        }
        let parent_components = path_components(parent_path)?;
        let parent = self.resolve_directory(&parent_components)?;
        let alias = short_name(short_alias)?;
        if self.find_entry(parent, &alias)?.is_some() {
            return Err(StorageError::AlreadyExists);
        }

        let size = u32::try_from(contents.len()).map_err(|_| StorageError::TooLarge)?;
        let cluster_size = self.cluster_size()?;
        let needed = contents.len().div_ceil(cluster_size);
        let chain = self.allocate_chain(needed)?;
        if let Err(error) = self.write_chain(&chain, contents) {
            let _ = self.release_clusters(&chain);
            return Err(error);
        }
        let locations = match self.find_two_free_slots(parent) {
            Ok(locations) => locations,
            Err(error) => {
                let _ = self.release_clusters(&chain);
                return Err(error);
            }
        };

        let mut lfn = [0xff_u8; 32];
        lfn[0] = 0x41;
        lfn[11] = ATTR_LONG_NAME;
        lfn[12] = 0;
        lfn[13] = short_name_checksum(&alias);
        lfn[26..28].fill(0);
        let mut name_units = [0xffff_u16; 13];
        name_units[..long_utf16.len()].copy_from_slice(&long_utf16);
        if long_utf16.len() < name_units.len() {
            name_units[long_utf16.len()] = 0;
        }
        encode_lfn_units(&mut lfn, &name_units);

        let mut short = [0_u8; 32];
        short[0..11].copy_from_slice(&alias);
        short[11] = ATTR_ARCHIVE;
        set_entry_cluster(&mut short, chain.first().copied().unwrap_or(0));
        short[28..32].copy_from_slice(&size.to_le_bytes());
        if let Err(error) = self
            .write_directory_entry(locations[0], &lfn)
            .and_then(|()| self.write_directory_entry(locations[1], &short))
        {
            let _ = self.release_clusters(&chain);
            return Err(error);
        }
        self.device.flush()
    }

    pub fn create_dir(&mut self, path: &str) -> Result<(), StorageError> {
        let components = path_components(path)?;
        if components.is_empty() {
            return Err(StorageError::InvalidName);
        }
        let (parent, name) = self.resolve_parent(&components)?;
        if self.find_entry(parent, &name)?.is_some() {
            return Err(StorageError::AlreadyExists);
        }
        let chain = self.allocate_chain(1)?;
        let cluster = chain[0];
        let mut contents = vec![0; self.cluster_size()?];
        let mut dot = [b' '; 11];
        dot[0] = b'.';
        contents[0..11].copy_from_slice(&dot);
        contents[11] = ATTR_DIRECTORY;
        set_entry_cluster(&mut contents[0..32], cluster);
        let mut dotdot = [b' '; 11];
        dotdot[0] = b'.';
        dotdot[1] = b'.';
        contents[32..43].copy_from_slice(&dotdot);
        contents[43] = ATTR_DIRECTORY;
        set_entry_cluster(&mut contents[32..64], parent);
        if let Err(error) = self.write_cluster(cluster, &contents) {
            let _ = self.release_clusters(&chain);
            return Err(error);
        }
        let location = match self.find_free_slot(parent) {
            Ok(location) => location,
            Err(error) => {
                let _ = self.release_clusters(&chain);
                return Err(error);
            }
        };
        let mut raw = [0_u8; 32];
        raw[0..11].copy_from_slice(&name);
        raw[11] = ATTR_DIRECTORY;
        set_entry_cluster(&mut raw, cluster);
        if let Err(error) = self.write_directory_entry(location, &raw) {
            let _ = self.release_clusters(&chain);
            return Err(error);
        }
        self.device.flush()
    }

    fn resolve_parent(&mut self, components: &[[u8; 11]]) -> Result<(u32, [u8; 11]), StorageError> {
        let (name, directories) = components.split_last().ok_or(StorageError::InvalidName)?;
        Ok((self.resolve_directory(directories)?, *name))
    }

    fn resolve_directory(&mut self, components: &[[u8; 11]]) -> Result<u32, StorageError> {
        let mut cluster = self.root_cluster;
        for component in components {
            let entry = self
                .find_entry(cluster, component)?
                .ok_or(StorageError::NotFound)?;
            if entry.bytes[11] & ATTR_DIRECTORY == 0 {
                return Err(StorageError::NotFound);
            }
            cluster = entry_cluster(&entry.bytes);
            if !self.valid_cluster(cluster) {
                return Err(StorageError::CorruptFilesystem);
            }
        }
        Ok(cluster)
    }

    fn read_directory(&mut self, cluster: u32) -> Result<Vec<RawDirectoryEntry>, StorageError> {
        let mut entries = Vec::new();
        for cluster in self.cluster_chain(cluster)? {
            let first_sector = self.cluster_lba(cluster)?;
            for sector_offset in 0..u64::from(self.sectors_per_cluster) {
                let sector_lba = first_sector
                    .checked_add(sector_offset)
                    .ok_or(StorageError::CorruptFilesystem)?;
                let sector = self.read_sector(sector_lba)?;
                for offset in (0..sector.len()).step_by(32) {
                    let marker = sector[offset];
                    if marker == 0 {
                        return Ok(entries);
                    }
                    if marker == 0xe5 {
                        continue;
                    }
                    let attributes = sector[offset + 11];
                    if attributes == ATTR_LONG_NAME || attributes & ATTR_VOLUME_ID != 0 {
                        continue;
                    }
                    entries.push(RawDirectoryEntry {
                        location: EntryLocation {
                            sector: sector_lba,
                            offset,
                        },
                        bytes: sector[offset..offset + 32]
                            .try_into()
                            .map_err(|_| StorageError::CorruptFilesystem)?,
                    });
                }
            }
        }
        Ok(entries)
    }

    fn find_two_free_slots(
        &mut self,
        directory_cluster: u32,
    ) -> Result<[EntryLocation; 2], StorageError> {
        for cluster in self.cluster_chain(directory_cluster)? {
            let first_sector = self.cluster_lba(cluster)?;
            for sector_offset in 0..u64::from(self.sectors_per_cluster) {
                let sector_lba = first_sector
                    .checked_add(sector_offset)
                    .ok_or(StorageError::CorruptFilesystem)?;
                let sector = self.read_sector(sector_lba)?;
                let mut first = None;
                for offset in (0..sector.len()).step_by(32) {
                    if matches!(sector[offset], 0 | 0xe5) {
                        let location = EntryLocation {
                            sector: sector_lba,
                            offset,
                        };
                        if let Some(first) = first {
                            return Ok([first, location]);
                        }
                        first = Some(location);
                    } else {
                        first = None;
                    }
                }
            }
        }
        Err(StorageError::NoSpace)
    }

    fn find_entry(
        &mut self,
        directory_cluster: u32,
        name: &[u8; 11],
    ) -> Result<Option<RawDirectoryEntry>, StorageError> {
        Ok(self
            .read_directory(directory_cluster)?
            .into_iter()
            .find(|entry| &entry.bytes[0..11] == name))
    }

    fn find_free_slot(&mut self, directory_cluster: u32) -> Result<EntryLocation, StorageError> {
        let chain = self.cluster_chain(directory_cluster)?;
        for cluster in chain.iter().copied() {
            let first_sector = self.cluster_lba(cluster)?;
            for sector_offset in 0..u64::from(self.sectors_per_cluster) {
                let sector_lba = first_sector
                    .checked_add(sector_offset)
                    .ok_or(StorageError::CorruptFilesystem)?;
                let sector = self.read_sector(sector_lba)?;
                for offset in (0..sector.len()).step_by(32) {
                    if matches!(sector[offset], 0 | 0xe5) {
                        return Ok(EntryLocation {
                            sector: sector_lba,
                            offset,
                        });
                    }
                }
            }
        }

        let extension = self.allocate_chain(1)?;
        let new_cluster = extension[0];
        let last_cluster = *chain.last().ok_or(StorageError::CorruptFilesystem)?;
        if let Err(error) = self.write_fat_entry(last_cluster, new_cluster) {
            let _ = self.release_clusters(&extension);
            return Err(error);
        }
        let zeroes = vec![0; self.cluster_size()?];
        self.write_cluster(new_cluster, &zeroes)?;
        Ok(EntryLocation {
            sector: self.cluster_lba(new_cluster)?,
            offset: 0,
        })
    }

    fn write_directory_entry(
        &mut self,
        location: EntryLocation,
        entry: &[u8; 32],
    ) -> Result<(), StorageError> {
        let mut sector = self.read_sector(location.sector)?;
        let end = location
            .offset
            .checked_add(32)
            .ok_or(StorageError::CorruptFilesystem)?;
        if end > sector.len() {
            return Err(StorageError::CorruptFilesystem);
        }
        sector[location.offset..end].copy_from_slice(entry);
        self.device.write_sectors(location.sector, &sector)
    }

    fn allocate_chain(&mut self, count: usize) -> Result<Vec<u32>, StorageError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut clusters = Vec::with_capacity(count);
        for cluster in 2..self.total_clusters + 2 {
            if self.read_fat_entry(cluster)? == 0 {
                self.write_fat_entry(cluster, FAT_EOC)?;
                clusters.push(cluster);
                if clusters.len() == count {
                    break;
                }
            }
        }
        if clusters.len() != count {
            let _ = self.release_clusters(&clusters);
            return Err(StorageError::NoSpace);
        }
        for pair in clusters.windows(2) {
            self.write_fat_entry(pair[0], pair[1])?;
        }
        Ok(clusters)
    }

    fn write_chain(&mut self, chain: &[u32], contents: &[u8]) -> Result<(), StorageError> {
        let cluster_size = self.cluster_size()?;
        if contents.len() > chain.len().saturating_mul(cluster_size) {
            return Err(StorageError::NoSpace);
        }
        for (index, cluster) in chain.iter().copied().enumerate() {
            let start = index
                .checked_mul(cluster_size)
                .ok_or(StorageError::TooLarge)?;
            let end = (start + cluster_size).min(contents.len());
            let mut bytes = vec![0; cluster_size];
            if start < contents.len() {
                bytes[..end - start].copy_from_slice(&contents[start..end]);
            }
            self.write_cluster(cluster, &bytes)?;
        }
        Ok(())
    }

    fn free_chain(&mut self, first_cluster: u32) -> Result<(), StorageError> {
        let chain = self.cluster_chain(first_cluster)?;
        self.release_clusters(&chain)
    }

    fn release_clusters(&mut self, clusters: &[u32]) -> Result<(), StorageError> {
        for cluster in clusters {
            self.write_fat_entry(*cluster, 0)?;
        }
        Ok(())
    }

    fn cluster_chain(&mut self, first_cluster: u32) -> Result<Vec<u32>, StorageError> {
        if !self.valid_cluster(first_cluster) {
            return Err(StorageError::CorruptFilesystem);
        }
        let mut chain = Vec::new();
        let mut current = first_cluster;
        for _ in 0..self.total_clusters {
            chain.push(current);
            let next = self.read_fat_entry(current)?;
            if next >= FAT_EOC_MIN {
                return Ok(chain);
            }
            if !self.valid_cluster(next) {
                return Err(StorageError::CorruptFilesystem);
            }
            current = next;
        }
        Err(StorageError::CorruptFilesystem)
    }

    fn read_fat_entry(&mut self, cluster: u32) -> Result<u32, StorageError> {
        if !self.valid_cluster(cluster) {
            return Err(StorageError::CorruptFilesystem);
        }
        let byte_offset = u64::from(cluster)
            .checked_mul(4)
            .ok_or(StorageError::CorruptFilesystem)?;
        let sector = u64::from(self.reserved_sectors)
            .checked_add(byte_offset / u64::from(self.bytes_per_sector))
            .ok_or(StorageError::CorruptFilesystem)?;
        let offset = usize::try_from(byte_offset % u64::from(self.bytes_per_sector))
            .map_err(|_| StorageError::CorruptFilesystem)?;
        let bytes = self.read_sector(sector)?;
        Ok(le_u32(&bytes, offset) & FAT_ENTRY_MASK)
    }

    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), StorageError> {
        if !self.valid_cluster(cluster) {
            return Err(StorageError::CorruptFilesystem);
        }
        let byte_offset = u64::from(cluster)
            .checked_mul(4)
            .ok_or(StorageError::CorruptFilesystem)?;
        let sector_in_fat = byte_offset / u64::from(self.bytes_per_sector);
        let offset = usize::try_from(byte_offset % u64::from(self.bytes_per_sector))
            .map_err(|_| StorageError::CorruptFilesystem)?;
        for fat_index in 0..self.fat_count {
            let sector = u64::from(self.reserved_sectors)
                .checked_add(
                    u64::from(fat_index)
                        .checked_mul(u64::from(self.sectors_per_fat))
                        .ok_or(StorageError::CorruptFilesystem)?,
                )
                .and_then(|base| base.checked_add(sector_in_fat))
                .ok_or(StorageError::CorruptFilesystem)?;
            let mut bytes = self.read_sector(sector)?;
            let previous = le_u32(&bytes, offset);
            let updated = (previous & !FAT_ENTRY_MASK) | (value & FAT_ENTRY_MASK);
            bytes[offset..offset + 4].copy_from_slice(&updated.to_le_bytes());
            self.device.write_sectors(sector, &bytes)?;
        }
        Ok(())
    }

    fn read_cluster(&mut self, cluster: u32) -> Result<Vec<u8>, StorageError> {
        let mut bytes = vec![0; self.cluster_size()?];
        self.device
            .read_sectors(self.cluster_lba(cluster)?, &mut bytes)?;
        Ok(bytes)
    }

    fn write_cluster(&mut self, cluster: u32, bytes: &[u8]) -> Result<(), StorageError> {
        if bytes.len() != self.cluster_size()? {
            return Err(StorageError::InvalidBuffer);
        }
        self.device.write_sectors(self.cluster_lba(cluster)?, bytes)
    }

    fn read_sector(&mut self, lba: u64) -> Result<Vec<u8>, StorageError> {
        let mut bytes = vec![0; usize::from(self.bytes_per_sector)];
        self.device.read_sectors(lba, &mut bytes)?;
        Ok(bytes)
    }

    fn cluster_lba(&self, cluster: u32) -> Result<u64, StorageError> {
        if !self.valid_cluster(cluster) {
            return Err(StorageError::CorruptFilesystem);
        }
        let relative = cluster
            .checked_sub(2)
            .and_then(|value| value.checked_mul(u32::from(self.sectors_per_cluster)))
            .ok_or(StorageError::CorruptFilesystem)?;
        u64::from(self.first_data_sector)
            .checked_add(u64::from(relative))
            .ok_or(StorageError::CorruptFilesystem)
    }

    fn cluster_size(&self) -> Result<usize, StorageError> {
        usize::from(self.bytes_per_sector)
            .checked_mul(usize::from(self.sectors_per_cluster))
            .ok_or(StorageError::TooLarge)
    }

    const fn valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster < self.total_clusters + 2
    }
}

fn path_components(path: &str) -> Result<Vec<[u8; 11]>, StorageError> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split('/')
        .map(short_name)
        .collect::<Result<Vec<_>, _>>()
}

fn short_name(name: &str) -> Result<[u8; 11], StorageError> {
    if name.is_empty() || matches!(name, "." | "..") || !name.is_ascii() {
        return Err(StorageError::InvalidName);
    }
    let mut parts = name.split('.');
    let base = parts.next().ok_or(StorageError::InvalidName)?;
    let extension = parts.next().unwrap_or("");
    if parts.next().is_some()
        || base.is_empty()
        || base.len() > 8
        || extension.len() > 3
        || !base
            .bytes()
            .chain(extension.bytes())
            .all(valid_short_character)
    {
        return Err(StorageError::InvalidName);
    }
    let mut output = [b' '; 11];
    for (destination, source) in output[..8].iter_mut().zip(base.bytes()) {
        *destination = source.to_ascii_uppercase();
    }
    for (destination, source) in output[8..].iter_mut().zip(extension.bytes()) {
        *destination = source.to_ascii_uppercase();
    }
    Ok(output)
}

fn valid_short_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"$%'-_@~`!(){}^#&".contains(&byte)
}

fn public_entry(bytes: &[u8; 32]) -> Result<FatDirectoryEntry, StorageError> {
    Ok(FatDirectoryEntry {
        name: display_short_name(&bytes[0..11])?,
        is_directory: bytes[11] & ATTR_DIRECTORY != 0,
        size: le_u32(bytes, 28),
        first_cluster: entry_cluster(bytes),
    })
}

fn display_short_name(bytes: &[u8]) -> Result<String, StorageError> {
    if bytes.len() != 11 {
        return Err(StorageError::CorruptFilesystem);
    }
    let base_end = bytes[..8]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    let extension_end = bytes[8..]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    let mut name = String::new();
    for byte in &bytes[..base_end] {
        name.push(char::from(*byte));
    }
    if extension_end != 0 {
        name.push('.');
        for byte in &bytes[8..8 + extension_end] {
            name.push(char::from(*byte));
        }
    }
    Ok(name)
}

fn entry_cluster(bytes: &[u8]) -> u32 {
    (u32::from(le_u16(bytes, 20)) << 16) | u32::from(le_u16(bytes, 26))
}

fn set_entry_cluster(bytes: &mut [u8], cluster: u32) {
    let cluster_bytes = cluster.to_le_bytes();
    bytes[20..22].copy_from_slice(&cluster_bytes[2..4]);
    bytes[26..28].copy_from_slice(&cluster_bytes[0..2]);
}

fn short_name_checksum(name: &[u8; 11]) -> u8 {
    name.iter().fold(0_u8, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(*byte)
    })
}

fn encode_lfn_units(entry: &mut [u8; 32], units: &[u16; 13]) {
    const OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    for (offset, unit) in OFFSETS.into_iter().zip(units) {
        entry[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::MemoryBlockDevice;

    #[test]
    fn mounts_and_updates_a_fat32_volume() {
        let mut bytes = vec![0; 64 * 1024 * 1024];
        fatfs::format_volume(
            Cursor::new(&mut bytes),
            fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32),
        )
        .unwrap();
        let disk = MemoryBlockDevice::from_bytes(bytes, 512).unwrap();
        let mut filesystem = Fat32::mount(disk).unwrap();

        filesystem
            .write_file("/HELLO.TXT", b"NexOS storage")
            .unwrap();
        assert_eq!(filesystem.read_file("HELLO.TXT").unwrap(), b"NexOS storage");
        filesystem.create_dir("/BOOT").unwrap();
        filesystem
            .write_file("/BOOT/KERNEL.BIN", &[0x4e, 0x45, 0x58])
            .unwrap();
        assert_eq!(
            filesystem.read_file("/BOOT/KERNEL.BIN").unwrap(),
            [0x4e, 0x45, 0x58]
        );
        let root = filesystem.list_dir("/").unwrap();
        assert!(root.iter().any(|entry| entry.name == "HELLO.TXT"));
        assert!(
            root.iter()
                .any(|entry| entry.name == "BOOT" && entry.is_directory)
        );
    }

    #[test]
    fn native_formatter_creates_writable_fat32() {
        let mut disk = MemoryBlockDevice::new(128 * 1024, 512).unwrap();
        let info = format_fat32(&mut disk, 0x4e45_584f).unwrap();
        assert_eq!(info.bytes_per_sector, 512);
        assert!(info.total_clusters >= FAT32_MIN_CLUSTERS);
        let mut filesystem = Fat32::mount(disk).unwrap();
        filesystem.create_dir("/EFI").unwrap();
        filesystem.create_dir("/EFI/BOOT").unwrap();
        filesystem
            .write_file("/EFI/BOOT/BOOTX64.EFI", b"efi")
            .unwrap();
        assert_eq!(
            filesystem.read_file("/EFI/BOOT/BOOTX64.EFI").unwrap(),
            b"efi"
        );
        filesystem
            .write_file_lfn("/EFI/BOOT/limine.conf", "LIMINE~1.CON", b"timeout: 0")
            .unwrap();
        assert_eq!(
            filesystem.read_file("/EFI/BOOT/LIMINE~1.CON").unwrap(),
            b"timeout: 0"
        );
    }

    #[test]
    fn rejects_names_that_need_long_file_name_entries() {
        assert_eq!(
            short_name("this-name-is-too-long.txt"),
            Err(StorageError::InvalidName)
        );
        assert_eq!(short_name("kernel.bin").unwrap(), *b"KERNEL  BIN");
    }
}
