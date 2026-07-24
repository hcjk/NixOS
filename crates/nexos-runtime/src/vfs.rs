use core::fmt;

use nexos_abi::Error;

pub const PATH_CAPACITY: usize = 256;
pub const NAME_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    RegularFile,
    Directory,
    BlockDevice,
    CharacterDevice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    pub id: NodeId,
    pub kind: NodeKind,
    pub size: u64,
    pub permissions: u16,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Path {
    bytes: [u8; PATH_CAPACITY],
    length: u16,
}

impl Path {
    #[must_use]
    pub const fn root() -> Self {
        let mut bytes = [0_u8; PATH_CAPACITY];
        bytes[0] = b'/';
        Self { bytes, length: 1 }
    }

    pub fn normalize(path: &[u8], current_directory: &[u8]) -> Result<Self, Error> {
        let mut output = Self {
            bytes: [0; PATH_CAPACITY],
            length: 0,
        };
        let mut component_starts = [0_u16; 64];
        let mut component_count = 0_usize;

        output.push_byte(b'/')?;
        if path.first() != Some(&b'/') {
            if current_directory.first() != Some(&b'/') {
                return Err(Error::InvalidArgument);
            }
            output.append_components(
                &current_directory[1..],
                &mut component_starts,
                &mut component_count,
            )?;
        }
        output.append_components(
            path.strip_prefix(b"/").unwrap_or(path),
            &mut component_starts,
            &mut component_count,
        )?;
        Ok(output)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.as_bytes() == b"/"
    }

    fn append_components(
        &mut self,
        input: &[u8],
        component_starts: &mut [u16; 64],
        component_count: &mut usize,
    ) -> Result<(), Error> {
        for component in input.split(|byte| *byte == b'/') {
            if component.is_empty() || component == b"." {
                continue;
            }
            if component == b".." {
                if *component_count != 0 {
                    *component_count -= 1;
                    self.length = component_starts[*component_count];
                    if self.length == 0 {
                        self.push_byte(b'/')?;
                    }
                }
                continue;
            }
            if component.len() > NAME_CAPACITY || component.contains(&0) {
                return Err(Error::NameTooLong);
            }
            if *component_count == component_starts.len() {
                return Err(Error::NameTooLong);
            }
            let component_start = self.length;
            if !self.is_root() {
                self.push_byte(b'/')?;
            }
            component_starts[*component_count] = component_start;
            *component_count += 1;
            self.push_bytes(component)?;
        }
        Ok(())
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), Error> {
        let index = usize::from(self.length);
        if index == PATH_CAPACITY {
            return Err(Error::NameTooLong);
        }
        self.bytes[index] = byte;
        self.length += 1;
        Ok(())
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        for byte in bytes {
            self.push_byte(*byte)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Path")
            .field(&self.as_bytes())
            .finish()
    }
}

pub trait FileSystem {
    fn root(&self) -> NodeMetadata;

    fn lookup(&self, parent: NodeId, name: &[u8]) -> Result<NodeMetadata, Error>;

    fn read(&self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, Error>;

    fn write(&self, node: NodeId, offset: u64, input: &[u8]) -> Result<usize, Error>;

    fn create(&self, parent: NodeId, name: &[u8], kind: NodeKind) -> Result<NodeMetadata, Error>;

    fn remove(&self, parent: NodeId, name: &[u8]) -> Result<(), Error>;
}

#[derive(Clone, Copy)]
struct Mount<'a> {
    path: Path,
    filesystem: &'a dyn FileSystem,
}

#[derive(Clone, Copy)]
pub struct ResolvedNode<'a> {
    pub filesystem: &'a dyn FileSystem,
    pub metadata: NodeMetadata,
}

pub struct Vfs<'a, const MOUNTS: usize> {
    mounts: [Option<Mount<'a>>; MOUNTS],
}

impl<'a, const MOUNTS: usize> Vfs<'a, MOUNTS> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mounts: [None; MOUNTS],
        }
    }

    pub fn mount(&mut self, path: &[u8], filesystem: &'a dyn FileSystem) -> Result<(), Error> {
        let path = Path::normalize(path, b"/")?;
        if self.mounts.iter().flatten().any(|mount| mount.path == path) {
            return Err(Error::AlreadyExists);
        }
        let slot = self
            .mounts
            .iter_mut()
            .find(|mount| mount.is_none())
            .ok_or(Error::MountLimit)?;
        *slot = Some(Mount { path, filesystem });
        Ok(())
    }

    pub fn unmount(&mut self, path: &[u8]) -> Result<(), Error> {
        let path = Path::normalize(path, b"/")?;
        let mount = self
            .mounts
            .iter_mut()
            .find(|mount| mount.is_some_and(|mount| mount.path == path))
            .ok_or(Error::NotFound)?;
        *mount = None;
        Ok(())
    }

    pub fn lookup(&self, path: &[u8], current_directory: &[u8]) -> Result<ResolvedNode<'a>, Error> {
        let path = Path::normalize(path, current_directory)?;
        let mount = self.select_mount(&path).ok_or(Error::NotFound)?;
        let mut node = mount.filesystem.root();
        let relative = relative_path(path.as_bytes(), mount.path.as_bytes());
        for component in relative.split(|byte| *byte == b'/') {
            if !component.is_empty() {
                if node.kind != NodeKind::Directory {
                    return Err(Error::NotDirectory);
                }
                node = mount.filesystem.lookup(node.id, component)?;
            }
        }
        Ok(ResolvedNode {
            filesystem: mount.filesystem,
            metadata: node,
        })
    }

    #[must_use]
    pub fn mount_count(&self) -> usize {
        self.mounts.iter().flatten().count()
    }

    fn select_mount(&self, path: &Path) -> Option<Mount<'a>> {
        self.mounts
            .iter()
            .flatten()
            .filter(|mount| path_has_prefix(path.as_bytes(), mount.path.as_bytes()))
            .max_by_key(|mount| mount.path.as_bytes().len())
            .copied()
    }
}

impl<const MOUNTS: usize> Default for Vfs<'_, MOUNTS> {
    fn default() -> Self {
        Self::new()
    }
}

fn path_has_prefix(path: &[u8], prefix: &[u8]) -> bool {
    prefix == b"/"
        || path == prefix
        || path
            .get(prefix.len())
            .is_some_and(|separator| path.starts_with(prefix) && *separator == b'/')
}

fn relative_path<'a>(path: &'a [u8], mount: &[u8]) -> &'a [u8] {
    if mount == b"/" {
        path.strip_prefix(b"/").unwrap_or(path)
    } else {
        path.get(mount.len()..)
            .unwrap_or_default()
            .strip_prefix(b"/")
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use super::*;

    struct TestFs {
        data: RefCell<[u8; 16]>,
    }

    impl FileSystem for TestFs {
        fn root(&self) -> NodeMetadata {
            NodeMetadata {
                id: NodeId(1),
                kind: NodeKind::Directory,
                size: 0,
                permissions: 0o755,
            }
        }

        fn lookup(&self, parent: NodeId, name: &[u8]) -> Result<NodeMetadata, Error> {
            if parent == NodeId(1) && name == b"hello" {
                Ok(NodeMetadata {
                    id: NodeId(2),
                    kind: NodeKind::RegularFile,
                    size: 5,
                    permissions: 0o644,
                })
            } else {
                Err(Error::NotFound)
            }
        }

        fn read(&self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
            if node != NodeId(2) {
                return Err(Error::NotFound);
            }
            let start = usize::try_from(offset).map_err(|_| Error::InvalidArgument)?;
            let data = self.data.borrow();
            let source = data.get(start..).ok_or(Error::InvalidArgument)?;
            let length = source.len().min(output.len());
            output[..length].copy_from_slice(&source[..length]);
            Ok(length)
        }

        fn write(&self, node: NodeId, offset: u64, input: &[u8]) -> Result<usize, Error> {
            if node != NodeId(2) {
                return Err(Error::NotFound);
            }
            let start = usize::try_from(offset).map_err(|_| Error::InvalidArgument)?;
            let mut data = self.data.borrow_mut();
            let destination = data.get_mut(start..).ok_or(Error::InvalidArgument)?;
            let length = destination.len().min(input.len());
            destination[..length].copy_from_slice(&input[..length]);
            Ok(length)
        }

        fn create(
            &self,
            _parent: NodeId,
            _name: &[u8],
            _kind: NodeKind,
        ) -> Result<NodeMetadata, Error> {
            Err(Error::ReadOnly)
        }

        fn remove(&self, _parent: NodeId, _name: &[u8]) -> Result<(), Error> {
            Err(Error::ReadOnly)
        }
    }

    #[test]
    fn normalizes_dot_and_parent_components() {
        let path = Path::normalize(b"../bin/./shell", b"/home/user").unwrap();
        assert_eq!(path.as_bytes(), b"/home/bin/shell");
        assert_eq!(Path::normalize(b"../../..", b"/").unwrap().as_bytes(), b"/");
    }

    #[test]
    fn longest_mount_prefix_wins() {
        let root = TestFs {
            data: RefCell::new(*b"hello world\0\0\0\0\0"),
        };
        let devices = TestFs {
            data: RefCell::new(*b"device data\0\0\0\0\0"),
        };
        let mut vfs = Vfs::<4>::new();
        vfs.mount(b"/", &root).unwrap();
        vfs.mount(b"/dev", &devices).unwrap();
        let node = vfs.lookup(b"/dev/hello", b"/").unwrap();
        assert!(core::ptr::eq(node.filesystem, &devices));
        assert_eq!(node.metadata.id, NodeId(2));
    }
}
