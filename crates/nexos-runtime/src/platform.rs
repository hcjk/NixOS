#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SleepTypes {
    pub primary: u16,
    pub secondary: u16,
}

#[must_use]
pub fn parse_s5_sleep_types(aml: &[u8]) -> Option<SleepTypes> {
    for name in aml
        .windows(4)
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == b"_S5_").then_some(index))
    {
        let package = name.checked_add(4)?;
        if aml.get(package).copied() != Some(0x12) {
            continue;
        }
        let (package_length, length_bytes) = decode_package_length(aml.get(package + 1..)?)?;
        let body = package.checked_add(1 + length_bytes)?;
        let package_end = package.checked_add(1 + package_length)?;
        if package_end > aml.len() || body >= package_end {
            continue;
        }
        let element_count = *aml.get(body)?;
        if element_count < 2 {
            continue;
        }
        let (primary, consumed) = parse_integer(aml.get(body + 1..package_end)?)?;
        let (secondary, _) = parse_integer(aml.get(body + 1 + consumed..package_end)?)?;
        return Some(SleepTypes {
            primary: u16::try_from(primary & 7).ok()?,
            secondary: u16::try_from(secondary & 7).ok()?,
        });
    }
    None
}

#[must_use]
pub fn ecam_function_address(
    base: u64,
    start_bus: u8,
    bus: u8,
    device: u8,
    function: u8,
) -> Option<u64> {
    if bus < start_bus || device >= 32 || function >= 8 || base & 0x000f_ffff != 0 {
        return None;
    }
    let bus_offset = u64::from(bus - start_bus).checked_shl(20)?;
    let device_offset = u64::from(device).checked_shl(15)?;
    let function_offset = u64::from(function).checked_shl(12)?;
    base.checked_add(bus_offset)?
        .checked_add(device_offset)?
        .checked_add(function_offset)
}

fn decode_package_length(bytes: &[u8]) -> Option<(usize, usize)> {
    let lead = *bytes.first()?;
    let following = usize::from(lead >> 6);
    if bytes.len() < following + 1 {
        return None;
    }
    let mut length = if following == 0 {
        usize::from(lead & 0x3f)
    } else {
        usize::from(lead & 0x0f)
    };
    for index in 0..following {
        length |= usize::from(bytes[index + 1]) << (4 + index * 8);
    }
    Some((length, following + 1))
}

fn parse_integer(bytes: &[u8]) -> Option<(u64, usize)> {
    match *bytes.first()? {
        0x00 => Some((0, 1)),
        0x01 => Some((1, 1)),
        0xff => Some((u64::MAX, 1)),
        0x0a => Some((u64::from(*bytes.get(1)?), 2)),
        0x0b => Some((
            u64::from(u16::from_le_bytes([*bytes.get(1)?, *bytes.get(2)?])),
            3,
        )),
        0x0c => Some((
            u64::from(u32::from_le_bytes([
                *bytes.get(1)?,
                *bytes.get(2)?,
                *bytes.get(3)?,
                *bytes.get(4)?,
            ])),
            5,
        )),
        0x0e => Some((
            u64::from_le_bytes([
                *bytes.get(1)?,
                *bytes.get(2)?,
                *bytes.get(3)?,
                *bytes.get(4)?,
                *bytes.get(5)?,
                *bytes.get(6)?,
                *bytes.get(7)?,
                *bytes.get(8)?,
            ]),
            9,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qemu_style_s5_package() {
        let aml = [
            0x08, b'_', b'S', b'5', b'_', 0x12, 0x07, 0x04, 0x0a, 0x05, 0x0a, 0x05, 0x00,
        ];
        assert_eq!(
            parse_s5_sleep_types(&aml),
            Some(SleepTypes {
                primary: 5,
                secondary: 5,
            })
        );
    }

    #[test]
    fn rejects_truncated_s5_package() {
        assert_eq!(parse_s5_sleep_types(b"_S5_\x12\x07\x02\x0a"), None);
    }

    #[test]
    fn computes_ecam_function_pages() {
        assert_eq!(
            ecam_function_address(0xe000_0000, 0, 2, 31, 7),
            Some(0xe02f_f000)
        );
        assert_eq!(ecam_function_address(0xe000_1000, 0, 0, 0, 0), None);
        assert_eq!(ecam_function_address(0xe000_0000, 4, 3, 0, 0), None);
    }
}
