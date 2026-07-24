use core::fmt;

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_VERSION_CURRENT: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_TYPE_SHARED: u16 = 3;
const ELF_MACHINE_X86_64: u16 = 0x3e;
const PROGRAM_TYPE_LOAD: u32 = 1;
const FLAG_EXECUTE: u32 = 1;
const FLAG_WRITE: u32 = 2;
const USER_ADDRESS_LIMIT: u64 = 0x0000_8000_0000_0000;
const MAX_PROGRAM_HEADERS: u16 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElfError {
    TruncatedHeader,
    BadMagic,
    UnsupportedClass,
    UnsupportedByteOrder,
    UnsupportedVersion,
    UnsupportedType,
    UnsupportedMachine,
    InvalidHeaderSize,
    InvalidProgramHeaderSize,
    InvalidProgramHeaderCount,
    ProgramHeaderTableOutOfBounds,
    SegmentOutOfBounds,
    SegmentAddressOverflow,
    SegmentOutsideUserSpace,
    InvalidSegmentAlignment,
    WritableExecutableSegment,
    MissingLoadSegment,
    EntryPointNotExecutable,
}

impl fmt::Display for ElfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadSegment<'a> {
    pub virtual_address: u64,
    pub memory_size: u64,
    pub file_data: &'a [u8],
    pub alignment: u64,
    pub writable: bool,
    pub executable: bool,
}

pub trait SegmentTarget {
    type Error;

    fn load_segment(&mut self, segment: LoadSegment<'_>) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError<T> {
    InvalidImage(ElfError),
    Target(T),
}

#[derive(Clone, Copy)]
pub struct ElfImage<'a> {
    bytes: &'a [u8],
    entry: u64,
    program_header_offset: usize,
    program_header_count: u16,
}

impl<'a> ElfImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < ELF_HEADER_SIZE {
            return Err(ElfError::TruncatedHeader);
        }
        if bytes[..4] != [0x7f, b'E', b'L', b'F'] {
            return Err(ElfError::BadMagic);
        }
        if bytes[4] != ELF_CLASS_64 {
            return Err(ElfError::UnsupportedClass);
        }
        if bytes[5] != ELF_DATA_LITTLE_ENDIAN {
            return Err(ElfError::UnsupportedByteOrder);
        }
        if bytes[6] != ELF_VERSION_CURRENT || read_u32(bytes, 20) != Some(1) {
            return Err(ElfError::UnsupportedVersion);
        }

        let elf_type = read_u16(bytes, 16).ok_or(ElfError::TruncatedHeader)?;
        if !matches!(elf_type, ELF_TYPE_EXECUTABLE | ELF_TYPE_SHARED) {
            return Err(ElfError::UnsupportedType);
        }
        if read_u16(bytes, 18) != Some(ELF_MACHINE_X86_64) {
            return Err(ElfError::UnsupportedMachine);
        }
        if read_u16(bytes, 52) != Some(64_u16) {
            return Err(ElfError::InvalidHeaderSize);
        }
        if read_u16(bytes, 54) != Some(56_u16) {
            return Err(ElfError::InvalidProgramHeaderSize);
        }

        let program_header_count = read_u16(bytes, 56).ok_or(ElfError::TruncatedHeader)?;
        if program_header_count == 0 || program_header_count > MAX_PROGRAM_HEADERS {
            return Err(ElfError::InvalidProgramHeaderCount);
        }
        let program_header_offset =
            usize::try_from(read_u64(bytes, 32).ok_or(ElfError::TruncatedHeader)?)
                .map_err(|_| ElfError::ProgramHeaderTableOutOfBounds)?;
        let table_size = usize::from(program_header_count)
            .checked_mul(PROGRAM_HEADER_SIZE)
            .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
        let table_end = program_header_offset
            .checked_add(table_size)
            .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
        if table_end > bytes.len() {
            return Err(ElfError::ProgramHeaderTableOutOfBounds);
        }

        let image = Self {
            bytes,
            entry: read_u64(bytes, 24).ok_or(ElfError::TruncatedHeader)?,
            program_header_offset,
            program_header_count,
        };
        image.validate_load_segments()?;
        Ok(image)
    }

    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.entry
    }

    #[must_use]
    pub const fn program_header_count(&self) -> u16 {
        self.program_header_count
    }

    #[must_use]
    pub fn load_segments(&self) -> LoadSegments<'a> {
        LoadSegments {
            image: *self,
            index: 0,
        }
    }

    pub fn load_into<T: SegmentTarget>(&self, target: &mut T) -> Result<u64, LoadError<T::Error>> {
        for index in 0..self.program_header_count {
            let segment = self.load_segment(index).map_err(LoadError::InvalidImage)?;
            if let Some(segment) = segment {
                target.load_segment(segment).map_err(LoadError::Target)?;
            }
        }
        Ok(self.entry)
    }

    fn validate_load_segments(&self) -> Result<(), ElfError> {
        let mut has_load_segment = false;
        let mut entry_is_executable = false;

        for index in 0..self.program_header_count {
            let Some(segment) = self.load_segment(index)? else {
                continue;
            };
            has_load_segment = true;
            if segment.executable
                && self.entry >= segment.virtual_address
                && self.entry
                    < segment
                        .virtual_address
                        .checked_add(segment.memory_size)
                        .ok_or(ElfError::SegmentAddressOverflow)?
            {
                entry_is_executable = true;
            }
        }

        if !has_load_segment {
            return Err(ElfError::MissingLoadSegment);
        }
        if !entry_is_executable {
            return Err(ElfError::EntryPointNotExecutable);
        }
        Ok(())
    }

    fn load_segment(&self, index: u16) -> Result<Option<LoadSegment<'a>>, ElfError> {
        let offset = self.program_header_offset + usize::from(index) * PROGRAM_HEADER_SIZE;
        if read_u32(self.bytes, offset) != Some(PROGRAM_TYPE_LOAD) {
            return Ok(None);
        }

        let flags = read_u32(self.bytes, offset + 4).ok_or(ElfError::SegmentOutOfBounds)?;
        let file_offset = read_u64(self.bytes, offset + 8).ok_or(ElfError::SegmentOutOfBounds)?;
        let virtual_address =
            read_u64(self.bytes, offset + 16).ok_or(ElfError::SegmentOutOfBounds)?;
        let file_size = read_u64(self.bytes, offset + 32).ok_or(ElfError::SegmentOutOfBounds)?;
        let memory_size = read_u64(self.bytes, offset + 40).ok_or(ElfError::SegmentOutOfBounds)?;
        let alignment = read_u64(self.bytes, offset + 48).ok_or(ElfError::SegmentOutOfBounds)?;

        if file_size > memory_size {
            return Err(ElfError::SegmentOutOfBounds);
        }
        let file_start = usize::try_from(file_offset).map_err(|_| ElfError::SegmentOutOfBounds)?;
        let file_length = usize::try_from(file_size).map_err(|_| ElfError::SegmentOutOfBounds)?;
        let file_end = file_start
            .checked_add(file_length)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        let file_data = self
            .bytes
            .get(file_start..file_end)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        let virtual_end = virtual_address
            .checked_add(memory_size)
            .ok_or(ElfError::SegmentAddressOverflow)?;
        if virtual_address >= USER_ADDRESS_LIMIT || virtual_end > USER_ADDRESS_LIMIT {
            return Err(ElfError::SegmentOutsideUserSpace);
        }
        if alignment > 1
            && (!alignment.is_power_of_two()
                || virtual_address % alignment != file_offset % alignment)
        {
            return Err(ElfError::InvalidSegmentAlignment);
        }

        let writable = flags & FLAG_WRITE != 0;
        let executable = flags & FLAG_EXECUTE != 0;
        if writable && executable {
            return Err(ElfError::WritableExecutableSegment);
        }

        Ok(Some(LoadSegment {
            virtual_address,
            memory_size,
            file_data,
            alignment,
            writable,
            executable,
        }))
    }
}

pub struct LoadSegments<'a> {
    image: ElfImage<'a>,
    index: u16,
}

impl<'a> Iterator for LoadSegments<'a> {
    type Item = LoadSegment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.image.program_header_count {
            let index = self.index;
            self.index += 1;
            match self.image.load_segment(index) {
                Ok(Some(segment)) => return Some(segment),
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        None
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let data: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(data))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let data: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(data))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let data: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable() -> [u8; 128] {
        let mut bytes = [0_u8; 128];
        bytes[..8].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x0040_0078_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());

        bytes[64..68].copy_from_slice(&PROGRAM_TYPE_LOAD.to_le_bytes());
        bytes[68..72].copy_from_slice(&FLAG_EXECUTE.to_le_bytes());
        bytes[72..80].copy_from_slice(&120_u64.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x0040_0078_u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&8_u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&16_u64.to_le_bytes());
        bytes[112..120].copy_from_slice(&8_u64.to_le_bytes());
        bytes[120..128].copy_from_slice(&[0x90, 0x90, 0x90, 0xc3, 1, 2, 3, 4]);
        bytes
    }

    #[test]
    fn parses_valid_x86_64_image() {
        let bytes = executable();
        let image = ElfImage::parse(&bytes).unwrap();
        assert_eq!(image.entry(), 0x0040_0078);
        let segment = image.load_segments().next().unwrap();
        assert_eq!(segment.memory_size, 16);
        assert!(segment.executable);
        assert!(!segment.writable);
    }

    #[test]
    fn rejects_writable_executable_segment() {
        let mut bytes = executable();
        bytes[68..72].copy_from_slice(&(FLAG_EXECUTE | FLAG_WRITE).to_le_bytes());
        assert!(matches!(
            ElfImage::parse(&bytes),
            Err(ElfError::WritableExecutableSegment)
        ));
    }

    #[test]
    fn rejects_entry_outside_executable_load_segment() {
        let mut bytes = executable();
        bytes[24..32].copy_from_slice(&0x0050_0000_u64.to_le_bytes());
        assert!(matches!(
            ElfImage::parse(&bytes),
            Err(ElfError::EntryPointNotExecutable)
        ));
    }

    #[test]
    fn rejects_truncated_program_header_table() {
        let mut bytes = executable();
        bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
        assert!(matches!(
            ElfImage::parse(&bytes),
            Err(ElfError::ProgramHeaderTableOutOfBounds)
        ));
    }

    #[test]
    fn loads_validated_segments_into_address_space_target() {
        struct Target {
            address: u64,
            file_bytes: usize,
            memory_bytes: u64,
        }

        impl SegmentTarget for Target {
            type Error = ();

            fn load_segment(&mut self, segment: LoadSegment<'_>) -> Result<(), Self::Error> {
                self.address = segment.virtual_address;
                self.file_bytes = segment.file_data.len();
                self.memory_bytes = segment.memory_size;
                Ok(())
            }
        }

        let bytes = executable();
        let image = ElfImage::parse(&bytes).unwrap();
        let mut target = Target {
            address: 0,
            file_bytes: 0,
            memory_bytes: 0,
        };
        assert_eq!(image.load_into(&mut target), Ok(0x0040_0078));
        assert_eq!(target.address, 0x0040_0078);
        assert_eq!(target.file_bytes, 8);
        assert_eq!(target.memory_bytes, 16);
    }
}
