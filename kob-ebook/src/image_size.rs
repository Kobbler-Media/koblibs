//! Intrinsic pixel dimensions of an image, read from its header bytes.
//!
//! Deliberately dependency-free and format-narrow: the full-page image
//! transform in [`crate::kepub`] needs a `viewBox` for the three raster
//! formats EPUBs actually carry (JPEG, PNG, GIF). Anything else returns
//! `None` and the caller leaves that page alone rather than guessing.

/// Read an image's intrinsic `(width, height)` in pixels, or `None` if the
/// bytes are not a format we understand or are truncated before the header.
pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    png(bytes).or_else(|| gif(bytes)).or_else(|| jpeg(bytes))
}

fn png(b: &[u8]) -> Option<(u32, u32)> {
    // 8-byte signature, then an IHDR chunk whose data starts at offset 16.
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    Some((be32(&b[16..20])?, be32(&b[20..24])?))
}

fn gif(b: &[u8]) -> Option<(u32, u32)> {
    // Logical screen width/height are little-endian u16 at offset 6.
    if b.len() < 10 || (&b[..6] != b"GIF87a" && &b[..6] != b"GIF89a") {
        return None;
    }
    Some((
        u16::from_le_bytes([b[6], b[7]]) as u32,
        u16::from_le_bytes([b[8], b[9]]) as u32,
    ))
}

/// Walk JPEG marker segments to the frame header (SOFn), which carries the
/// dimensions. SOF0/1/2/3/5/6/7/9/10/11/13/14/15 are all frame headers;
/// C4/C8/CC are DHT/JPG/DAC and must be skipped like any other segment.
fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    loop {
        // Segments are 0xFF then a marker; fill bytes (0xFF) may repeat.
        while i < b.len() && b[i] == 0xFF {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;

        // Standalone markers carry no length payload.
        if matches!(marker, 0xD8 | 0xD9) || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        // Start of scan — compressed data follows, no header left to find.
        if marker == 0xDA {
            return None;
        }

        let len = u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        let is_frame_header =
            matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_frame_header {
            // length(2) precision(1) height(2) width(2)
            let h = u16::from_be_bytes([*b.get(i + 3)?, *b.get(i + 4)?]) as u32;
            let w = u16::from_be_bytes([*b.get(i + 5)?, *b.get(i + 6)?]) as u32;
            return (w > 0 && h > 0).then_some((w, h));
        }
        i += len;
    }
}

fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.first()?,
        *b.get(1)?,
        *b.get(2)?,
        *b.get(3)?,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_png() {
        let mut b = Vec::from(*b"\x89PNG\r\n\x1a\n");
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&1638u32.to_be_bytes());
        b.extend_from_slice(&2441u32.to_be_bytes());
        assert_eq!(dimensions(&b), Some((1638, 2441)));
    }

    #[test]
    fn reads_gif() {
        let mut b = Vec::from(*b"GIF89a");
        b.extend_from_slice(&2596u16.to_le_bytes());
        b.extend_from_slice(&1540u16.to_le_bytes());
        assert_eq!(dimensions(&b), Some((2596, 1540)));
    }

    #[test]
    fn reads_jpeg_skipping_segments() {
        let mut b = vec![0xFF, 0xD8];
        // A JFIF APP0 segment to step over before the frame header.
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        // SOF0: len=11, precision=8, height=335, width=362
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        b.extend_from_slice(&335u16.to_be_bytes());
        b.extend_from_slice(&362u16.to_be_bytes());
        b.extend_from_slice(&[0x03, 0x01]);
        assert_eq!(dimensions(&b), Some((362, 335)));
    }

    #[test]
    fn jpeg_does_not_mistake_a_huffman_table_for_a_frame() {
        let mut b = vec![0xFF, 0xD8];
        // DHT (0xC4) is inside the 0xC0..=0xCF range but is not a frame header.
        b.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x05, 0x00, 0x00, 0x00]);
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        b.extend_from_slice(&100u16.to_be_bytes());
        b.extend_from_slice(&200u16.to_be_bytes());
        assert_eq!(dimensions(&b), Some((200, 100)));
    }

    #[test]
    fn rejects_unknown_and_truncated() {
        assert_eq!(dimensions(b"not an image"), None);
        assert_eq!(dimensions(b"\x89PNG\r\n\x1a\n\x00\x00"), None);
        assert_eq!(dimensions(&[]), None);
    }
}
