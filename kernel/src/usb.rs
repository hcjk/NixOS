use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;
use crate::pci::PciInventory;
use crate::xhci::{
    MAX_USB_STORAGE_DEVICES, ProbeStats, UsbInputEvent, UsbMassStorageDevice, XhciController,
    XhciManager,
};
use nexos_usb::hid::MouseReport;

pub struct UsbManager {
    xhci: XhciManager,
    latest_mouse: Option<MouseReport>,
    mouse_events: u64,
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
            latest_mouse: None,
            mouse_events: 0,
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

    pub fn poll_keyboard_character(&mut self) -> Option<u8> {
        match self.xhci.poll_input()? {
            UsbInputEvent::Keyboard(event) => {
                if event.control
                    && event
                        .ascii
                        .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'c'))
                {
                    Some(3)
                } else {
                    event.ascii
                }
            }
            UsbInputEvent::Mouse(event) => {
                self.latest_mouse = Some(event);
                self.mouse_events = self.mouse_events.saturating_add(1);
                None
            }
        }
    }

    #[must_use]
    pub const fn latest_mouse(&self) -> Option<MouseReport> {
        self.latest_mouse
    }

    #[must_use]
    pub const fn mouse_events(&self) -> u64 {
        self.mouse_events
    }

    pub fn mass_storage_devices(
        &mut self,
        output: &mut [Option<UsbMassStorageDevice>; MAX_USB_STORAGE_DEVICES],
    ) -> usize {
        self.xhci.mass_storage_devices(output)
    }
}
