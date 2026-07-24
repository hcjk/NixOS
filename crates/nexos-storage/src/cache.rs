use alloc::vec;
use alloc::vec::Vec;

use crate::{BlockDevice, StorageError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub writebacks: u64,
}

struct CacheEntry {
    lba: u64,
    bytes: Vec<u8>,
    dirty: bool,
    stamp: u64,
}

pub struct CachedBlockDevice<D> {
    inner: D,
    entries: Vec<CacheEntry>,
    capacity: usize,
    stamp: u64,
    stats: CacheStats,
}

impl<D: BlockDevice> CachedBlockDevice<D> {
    pub fn new(inner: D, capacity_sectors: usize) -> Result<Self, StorageError> {
        if capacity_sectors == 0 {
            return Err(StorageError::InvalidBuffer);
        }
        Ok(Self {
            inner,
            entries: Vec::with_capacity(capacity_sectors),
            capacity: capacity_sectors,
            stamp: 0,
            stats: CacheStats::default(),
        })
    }

    #[must_use]
    pub const fn stats(&self) -> CacheStats {
        self.stats
    }

    #[must_use]
    pub fn inner(&self) -> &D {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    pub fn into_inner(mut self) -> Result<D, StorageError> {
        self.flush()?;
        Ok(self.inner)
    }

    fn touch(&mut self, index: usize) {
        self.stamp = self.stamp.wrapping_add(1);
        self.entries[index].stamp = self.stamp;
    }

    fn entry_for(&mut self, lba: u64) -> Result<usize, StorageError> {
        if let Some(index) = self.entries.iter().position(|entry| entry.lba == lba) {
            self.stats.hits = self.stats.hits.saturating_add(1);
            self.touch(index);
            return Ok(index);
        }

        self.stats.misses = self.stats.misses.saturating_add(1);
        let sector_size =
            usize::try_from(self.inner.sector_size()).map_err(|_| StorageError::TooLarge)?;
        let index = if self.entries.len() < self.capacity {
            self.entries.push(CacheEntry {
                lba,
                bytes: vec![0; sector_size],
                dirty: false,
                stamp: 0,
            });
            self.entries.len() - 1
        } else {
            let index = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(index, _)| index)
                .ok_or(StorageError::Device)?;
            self.write_back(index)?;
            self.stats.evictions = self.stats.evictions.saturating_add(1);
            self.entries[index].lba = lba;
            index
        };

        self.inner
            .read_sectors(lba, &mut self.entries[index].bytes)?;
        self.entries[index].dirty = false;
        self.touch(index);
        Ok(index)
    }

    fn write_back(&mut self, index: usize) -> Result<(), StorageError> {
        if self.entries[index].dirty {
            let entry = &mut self.entries[index];
            self.inner.write_sectors(entry.lba, &entry.bytes)?;
            entry.dirty = false;
            self.stats.writebacks = self.stats.writebacks.saturating_add(1);
        }
        Ok(())
    }

    fn validate_transfer(&self, lba: u64, length: usize) -> Result<usize, StorageError> {
        let sector_size =
            usize::try_from(self.inner.sector_size()).map_err(|_| StorageError::TooLarge)?;
        if length == 0 || !length.is_multiple_of(sector_size) {
            return Err(StorageError::InvalidBuffer);
        }
        let sectors = length / sector_size;
        let sectors_u64 = u64::try_from(sectors).map_err(|_| StorageError::TooLarge)?;
        if lba
            .checked_add(sectors_u64)
            .is_none_or(|end| end > self.inner.sector_count())
        {
            return Err(StorageError::OutOfBounds);
        }
        Ok(sector_size)
    }
}

impl<D: BlockDevice> BlockDevice for CachedBlockDevice<D> {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        let sector_size = self.validate_transfer(lba, output.len())?;
        for (offset, destination) in output.chunks_exact_mut(sector_size).enumerate() {
            let sector_lba = lba
                .checked_add(u64::try_from(offset).map_err(|_| StorageError::TooLarge)?)
                .ok_or(StorageError::OutOfBounds)?;
            let index = self.entry_for(sector_lba)?;
            destination.copy_from_slice(&self.entries[index].bytes);
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        let sector_size = self.validate_transfer(lba, input.len())?;
        for (offset, source) in input.chunks_exact(sector_size).enumerate() {
            let sector_lba = lba
                .checked_add(u64::try_from(offset).map_err(|_| StorageError::TooLarge)?)
                .ok_or(StorageError::OutOfBounds)?;
            let index = self.entry_for(sector_lba)?;
            self.entries[index].bytes.copy_from_slice(source);
            self.entries[index].dirty = true;
            self.touch(index);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        for index in 0..self.entries.len() {
            self.write_back(index)?;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    #[test]
    fn write_back_cache_tracks_hits_and_evictions() {
        let disk = MemoryBlockDevice::new(8, 512).unwrap();
        let mut cache = CachedBlockDevice::new(disk, 2).unwrap();
        cache.write_sectors(1, &[0x11; 512]).unwrap();
        let mut first = [0; 512];
        cache.read_sectors(1, &mut first).unwrap();
        assert_eq!(first, [0x11; 512]);
        cache.read_sectors(2, &mut [0; 512]).unwrap();
        cache.read_sectors(3, &mut [0; 512]).unwrap();
        assert!(cache.stats().hits >= 1);
        assert_eq!(cache.stats().evictions, 1);

        let mut disk = cache.into_inner().unwrap();
        let mut persisted = [0; 512];
        disk.read_sectors(1, &mut persisted).unwrap();
        assert_eq!(persisted, [0x11; 512]);
    }
}
