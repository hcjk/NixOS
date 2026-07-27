use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;
use crate::pci::PciInventory;
use crate::xhci::{ProbeStats, XhciController, XhciManager};

pub struct UsbManager {
    xhci: XhciManager,
}

impl UsbManager {
    #[must_use]
    pub fn discover(
        inventory: &PciInventory,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Self {
        Self {
            xhci: XhciManager::discover(inventory, paging, allocator),
        }
    }

    pub fn refresh(&mut self) {
        self.xhci.refresh();
    }

    #[must_use]
    pub const fn controller_count(&self) -> usize {
        self.xhci.count()
    }

    #[must_use]
    pub const fn stats(&self) -> ProbeStats {
        self.xhci.stats()
    }

    #[must_use]
    pub fn controller(&self, index: usize) -> Option<&XhciController> {
        self.xhci.controller(index)
    }

    pub fn self_test(&mut self) -> bool {
        self.xhci.self_test()
    }
}
