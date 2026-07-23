use core::arch::asm;

const ENTRY_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const PRESENT: u64 = 1;
const HUGE_PAGE: u64 = 1 << 7;

#[derive(Clone, Copy)]
pub struct Mapping {
    pub physical_address: u64,
    pub page_size: u64,
    pub flags: u64,
}

pub struct PagingInfo {
    hhdm_offset: u64,
    level_4_frame: u64,
}

impl PagingInfo {
    #[must_use]
    pub fn detect(hhdm_offset: u64) -> Self {
        let cr3: u64;
        // SAFETY: Reading CR3 is valid at CPL0.
        unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
        Self {
            hhdm_offset,
            level_4_frame: cr3 & ENTRY_ADDRESS_MASK,
        }
    }

    #[must_use]
    pub const fn hhdm_offset(&self) -> u64 {
        self.hhdm_offset
    }

    #[must_use]
    pub const fn level_4_frame(&self) -> u64 {
        self.level_4_frame
    }

    #[must_use]
    pub fn translate(&self, virtual_address: u64) -> Option<Mapping> {
        let indices = [
            (virtual_address >> 39) & 0x1ff,
            (virtual_address >> 30) & 0x1ff,
            (virtual_address >> 21) & 0x1ff,
            (virtual_address >> 12) & 0x1ff,
        ];
        let mut table = self.level_4_frame;

        for (level, index) in indices.into_iter().enumerate() {
            let entry = self.read_entry(table, index)?;
            if entry & PRESENT == 0 {
                return None;
            }
            let frame = entry & ENTRY_ADDRESS_MASK;
            if level == 1 && entry & HUGE_PAGE != 0 {
                let page_size = 1024 * 1024 * 1024;
                return Some(Mapping {
                    physical_address: frame + (virtual_address & (page_size - 1)),
                    page_size,
                    flags: entry,
                });
            }
            if level == 2 && entry & HUGE_PAGE != 0 {
                let page_size = 2 * 1024 * 1024;
                return Some(Mapping {
                    physical_address: frame + (virtual_address & (page_size - 1)),
                    page_size,
                    flags: entry,
                });
            }
            if level == 3 {
                return Some(Mapping {
                    physical_address: frame + (virtual_address & 0xfff),
                    page_size: 4096,
                    flags: entry,
                });
            }
            table = frame;
        }
        None
    }

    fn read_entry(&self, table_frame: u64, index: u64) -> Option<u64> {
        let address = self
            .hhdm_offset
            .checked_add(table_frame)?
            .checked_add(index.checked_mul(8)?)?;
        // SAFETY: Limine's HHDM maps physical memory, and each table frame came
        // from a present page-table entry or CR3. The index is restricted to
        // nine bits.
        Some(unsafe { core::ptr::read_volatile(address as *const u64) })
    }
}
