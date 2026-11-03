//! Just enough EXIF (TIFF) handling to keep a photo's orientation when stripping metadata.
//! Everything else in EXIF (GPS, camera, dates, serial numbers) is dropped.

const ORIENTATION_TAG: u16 = 0x0112;
const TYPE_SHORT: u16 = 3;

/// Orientation (2..=8) from TIFF-structured EXIF data, or `None` when absent or normal (1).
pub fn orientation(tiff: &[u8]) -> Option<u16> {
    let big_endian = match tiff.get(0..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).find_map(|i| {
        let entry = ifd + 2 + i * 12;
        (u16_at(entry)? == ORIENTATION_TAG && u16_at(entry + 2)? == TYPE_SHORT)
            .then(|| u16_at(entry + 8))
            .flatten()
            .filter(|o| (2..=8).contains(o))
    })
}

/// A minimal big-endian TIFF block holding only the orientation tag.
pub fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 right after the header
    tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
    tiff.extend_from_slice(&ORIENTATION_TAG.to_be_bytes());
    tiff.extend_from_slice(&TYPE_SHORT.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes()); // count
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]); // value padding
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    tiff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_orientation() {
        for o in 2..=8 {
            assert_eq!(orientation(&orientation_only(o)), Some(o));
        }
        assert_eq!(orientation(&orientation_only(1)), None, "normal orientation needs no tag");
    }

    #[test]
    fn reads_little_endian_with_other_tags() {
        // II header, IFD0 at 8 with two entries: Make (0x010F, ASCII) then Orientation = 6.
        let mut t = b"II*\0\x08\0\0\0\x02\0".to_vec();
        t.extend_from_slice(&[0x0F, 0x01, 2, 0, 4, 0, 0, 0, b'A', b'B', b'C', 0]);
        t.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        t.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(orientation(&t), Some(6));
    }

    #[test]
    fn ignores_garbage() {
        assert_eq!(orientation(b""), None);
        assert_eq!(orientation(b"MM\0*\xFF\xFF\xFF\xFF"), None);
    }
}
