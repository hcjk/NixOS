use core::fmt;

use nexos_abi::Error;

pub const CURRENT_DIRECTORY_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Ready,
    Running,
    Waiting { child: Option<ProcessId> },
    Exited { status: i32 },
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => formatter.write_str("ready"),
            Self::Running => formatter.write_str("running"),
            Self::Waiting { child: Some(child) } => {
                write!(formatter, "waiting({})", child.0)
            }
            Self::Waiting { child: None } => formatter.write_str("waiting(any)"),
            Self::Exited { status } => write!(formatter, "exited({status})"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handle {
    pub object: u32,
    pub rights: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Process<const HANDLES: usize> {
    pub id: ProcessId,
    pub parent: Option<ProcessId>,
    pub state: ProcessState,
    pub address_space: u64,
    pub entry_point: u64,
    pub user_stack: u64,
    handles: [Option<Handle>; HANDLES],
    current_directory: [u8; CURRENT_DIRECTORY_CAPACITY],
    current_directory_length: u8,
}

impl<const HANDLES: usize> Process<HANDLES> {
    #[must_use]
    pub fn current_directory(&self) -> &[u8] {
        &self.current_directory[..usize::from(self.current_directory_length)]
    }

    #[must_use]
    pub fn handle(&self, descriptor: usize) -> Option<Handle> {
        self.handles.get(descriptor).copied().flatten()
    }
}

pub struct ProcessTable<const PROCESSES: usize, const HANDLES: usize> {
    processes: [Option<Process<HANDLES>>; PROCESSES],
    next_id: u32,
}

impl<const PROCESSES: usize, const HANDLES: usize> ProcessTable<PROCESSES, HANDLES> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            processes: [None; PROCESSES],
            next_id: 1,
        }
    }

    pub fn create(
        &mut self,
        parent: Option<ProcessId>,
        address_space: u64,
        entry_point: u64,
        user_stack: u64,
    ) -> Result<ProcessId, Error> {
        if entry_point == 0 || user_stack == 0 {
            return Err(Error::InvalidArgument);
        }
        if parent.is_some_and(|id| self.get(id).is_none()) {
            return Err(Error::NotFound);
        }
        let slot = self
            .processes
            .iter()
            .position(Option::is_none)
            .ok_or(Error::ProcessLimit)?;
        let id = ProcessId(self.next_id);
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let mut current_directory = [0_u8; CURRENT_DIRECTORY_CAPACITY];
        current_directory[0] = b'/';
        self.processes[slot] = Some(Process {
            id,
            parent,
            state: ProcessState::Ready,
            address_space,
            entry_point,
            user_stack,
            handles: [None; HANDLES],
            current_directory,
            current_directory_length: 1,
        });
        Ok(id)
    }

    #[must_use]
    pub fn get(&self, id: ProcessId) -> Option<&Process<HANDLES>> {
        self.processes
            .iter()
            .flatten()
            .find(|process| process.id == id)
    }

    pub fn get_mut(&mut self, id: ProcessId) -> Option<&mut Process<HANDLES>> {
        self.processes
            .iter_mut()
            .flatten()
            .find(|process| process.id == id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.processes.iter().flatten().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_state(&mut self, id: ProcessId, state: ProcessState) -> Result<(), Error> {
        self.get_mut(id).ok_or(Error::NotFound)?.state = state;
        Ok(())
    }

    pub fn set_current_directory(&mut self, id: ProcessId, path: &[u8]) -> Result<(), Error> {
        if path.is_empty()
            || path[0] != b'/'
            || path.len() >= CURRENT_DIRECTORY_CAPACITY
            || path.contains(&0)
        {
            return Err(Error::InvalidArgument);
        }
        let process = self.get_mut(id).ok_or(Error::NotFound)?;
        process.current_directory.fill(0);
        process.current_directory[..path.len()].copy_from_slice(path);
        process.current_directory_length =
            u8::try_from(path.len()).map_err(|_| Error::NameTooLong)?;
        Ok(())
    }

    pub fn allocate_handle(&mut self, id: ProcessId, handle: Handle) -> Result<usize, Error> {
        let process = self.get_mut(id).ok_or(Error::NotFound)?;
        let descriptor = process
            .handles
            .iter()
            .position(Option::is_none)
            .ok_or(Error::HandleLimit)?;
        process.handles[descriptor] = Some(handle);
        Ok(descriptor)
    }

    pub fn close_handle(&mut self, id: ProcessId, descriptor: usize) -> Result<Handle, Error> {
        let process = self.get_mut(id).ok_or(Error::NotFound)?;
        let slot = process
            .handles
            .get_mut(descriptor)
            .ok_or(Error::BadHandle)?;
        slot.take().ok_or(Error::BadHandle)
    }

    pub fn exit(&mut self, id: ProcessId, status: i32) -> Result<(), Error> {
        let process = self.get_mut(id).ok_or(Error::NotFound)?;
        process.state = ProcessState::Exited { status };
        process.handles.fill(None);
        Ok(())
    }

    pub fn reap(&mut self, parent: ProcessId, child: ProcessId) -> Result<i32, Error> {
        let slot = self
            .processes
            .iter()
            .position(|process| {
                process.is_some_and(|process| process.id == child && process.parent == Some(parent))
            })
            .ok_or(Error::NoChild)?;
        let process = self.processes[slot].ok_or(Error::NoChild)?;
        let ProcessState::Exited { status } = process.state else {
            return Err(Error::WouldBlock);
        };
        self.processes[slot] = None;
        Ok(status)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Process<HANDLES>> {
        self.processes.iter().flatten()
    }
}

impl<const PROCESSES: usize, const HANDLES: usize> Default for ProcessTable<PROCESSES, HANDLES> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_processes_and_reaps_children() {
        let mut table = ProcessTable::<4, 4>::new();
        let parent = table
            .create(None, 0x1000, 0x0040_0000, 0x0080_0000)
            .unwrap();
        let child = table
            .create(Some(parent), 0x2000, 0x0050_0000, 0x0090_0000)
            .unwrap();
        table.exit(child, 23).unwrap();
        assert_eq!(table.reap(parent, child), Ok(23));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn allocates_lowest_available_descriptor() {
        let mut table = ProcessTable::<1, 2>::new();
        let process = table.create(None, 1, 2, 3).unwrap();
        let handle = Handle {
            object: 10,
            rights: 3,
        };
        assert_eq!(table.allocate_handle(process, handle), Ok(0));
        assert_eq!(table.allocate_handle(process, handle), Ok(1));
        assert_eq!(
            table.allocate_handle(process, handle),
            Err(Error::HandleLimit)
        );
        assert_eq!(table.close_handle(process, 0), Ok(handle));
        assert_eq!(table.allocate_handle(process, handle), Ok(0));
    }
}
