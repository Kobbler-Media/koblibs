use std::str::FromStr;

use bytes::Bytes;

/// Upper bound on one decoded article; real articles are around 1 MiB.
pub const MAX_DECODED_LEN: usize = 32 * 1024 * 1024;

/// A decoded yEnc article body.
#[derive(Debug, Clone)]
pub struct Body {
    pub data: Bytes,
    /// Zero-based offset of `data` within the file.
    pub offset: u64,
    /// Size of the whole file from `=ybegin size=`.
    pub file_size: u64,
    /// One-based part number, for multipart posts.
    pub part: Option<u32>,
    /// Number of parts the file was posted in, from `=ybegin total=`.
    pub total: Option<u32>,
    /// CRC-32 of the file name from `=ybegin name=`.
    pub name_crc: Option<u32>,
}

pub(crate) const MISMATCH: crate::vendor::nntp::Error =
    crate::vendor::nntp::Error::Yenc("article does not match imported geometry");

/// Geometry independently known from the imported source. Checked before a body
/// can reach a caller or the abandoned-body cache sink.
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyExpectation {
    pub part: Option<u32>,
    pub total: Option<u32>,
    pub file_size: Option<u64>,
    pub offset: Option<u64>,
    pub decoded_len: Option<u64>,
    /// When the article was posted, in Unix seconds. Not checked against the body:
    /// providers whose retention it is past are not asked.
    pub posted: Option<i64>,
    /// CRC-32 of the `=ybegin name=`; checked only before a body streams.
    pub name_crc: Option<u32>,
    /// Part number the layout's equal-sized parts imply; checked only before a body
    /// streams, since a poster's numbering may disagree with a layout that sums right.
    pub layout_part: Option<u32>,
}
impl BodyExpectation {
    /// Nothing known: every body passes.
    pub const NONE: Self = Self {
        part: None,
        total: None,
        file_size: None,
        offset: None,
        decoded_len: None,
        posted: None,
        name_crc: None,
        layout_part: None,
    };

    /// Only the expected `=ypart` offset, when known.
    #[must_use]
    pub const fn at(offset: Option<u64>) -> Self {
        Self {
            offset,
            ..Self::NONE
        }
    }

    /// Whether a `=ybegin` header already shows another file or part, before any data.
    pub(crate) fn rejects_header(self, file_size: u64, part: Option<u32>) -> bool {
        self.file_size.is_some_and(|v| v != file_size)
            || self
                .part
                .is_some_and(|v| part.is_some_and(|actual| actual != v))
    }

    /// Whether the `=ybegin` and `=ypart` (`range`, as offset and length) headers name
    /// exactly the article the layout places here, so its bytes may stream before the
    /// CRC is checked. Needs the layout's offset and length.
    pub(crate) fn admits_stream(
        self,
        file_size: u64,
        part: Option<u32>,
        name_crc: Option<u32>,
        range: Option<(u64, usize)>,
    ) -> bool {
        let (begin, len) = match (part, range) {
            (_, Some(range)) => range,
            (None, None) => (0, usize::try_from(file_size).unwrap_or(usize::MAX)),
            (Some(_), None) => return false,
        };
        self.offset == Some(begin)
            && self.decoded_len == Some(len as u64)
            && self.file_size.is_none_or(|v| v == file_size)
            && self.part.is_none_or(|v| part == Some(v))
            && self.layout_part.is_none_or(|v| part == Some(v))
            && self.name_crc.is_none_or(|v| name_crc == Some(v))
    }

    pub(crate) fn validate(self, body: &Body) -> Result<(), crate::vendor::nntp::Error> {
        let regular = match body.part {
            Some(1) if body.offset == 0 => Some(body.data.len() as u64),
            Some(part)
                if part > 1
                    && body.offset > 0
                    && body.offset.is_multiple_of(u64::from(part - 1)) =>
            {
                Some(body.offset / u64::from(part - 1))
            }
            _ => None,
        };
        let obfuscated_total = self.part == body.part
            && regular.is_some_and(|size| {
                size > 0
                    && body.data.len() as u64 <= size
                    && body
                        .total
                        .is_some_and(|total| body.file_size.div_ceil(size) != u64::from(total))
            });
        let impossible_range = self.part.is_some()
            && body.part.is_some()
            && self.offset.is_none()
            && match self.part {
                Some(1) => body.offset != 0,
                Some(_) => {
                    body.offset == 0
                        || (body.file_size > 0
                            && body.offset.saturating_add(body.data.len() as u64) > body.file_size)
                }
                None => false,
            };
        if self
            .part
            .is_some_and(|v| body.part.is_some_and(|actual| actual != v))
            || impossible_range
            || (!obfuscated_total
                && self
                    .total
                    .is_some_and(|v| body.total.is_some_and(|actual| actual != v)))
            || self.file_size.is_some_and(|v| body.file_size != v)
            || self.offset.is_some_and(|v| body.offset != v)
            || self
                .decoded_len
                .is_some_and(|v| body.data.len() as u64 != v)
        {
            return Err(MISMATCH);
        }
        Ok(())
    }
}

/// Value of `key=` in a `=y…` header line. Stops at `name=`, which is always last and
/// may contain spaces.
fn raw_field<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a str> {
    let value = line
        .trim_ascii_end()
        .split(|&b| b == b' ')
        .take_while(|token| !token.starts_with(b"name="))
        .find_map(|token| token.strip_prefix(key)?.strip_prefix(b"="))?;
    std::str::from_utf8(value).ok()
}

/// CRC-32 of the `name=` value that ends a `=ybegin` line, trailing whitespace excluded.
pub fn name_crc(line: &[u8]) -> Option<u32> {
    let at = line.windows(6).position(|w| w == b" name=")? + 6;
    Some(crate::vendor::yenc::crc32(0, line[at..].trim_ascii_end()))
}

pub fn field<T: FromStr>(line: &[u8], key: &[u8]) -> Option<T> {
    raw_field(line, key)?.parse().ok()
}

pub fn crc_field(line: &[u8], key: &[u8]) -> Option<u32> {
    u32::from_str_radix(raw_field(line, key)?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_stop_at_name() {
        let line = b"=ybegin part=2 line=128 size=15 name=a size=9 b.bin\r\n";
        assert_eq!(field::<u64>(line, b"size"), Some(15));
        assert_eq!(field::<u32>(line, b"part"), Some(2));
        assert_eq!(
            crc_field(b"end size=5 pcrc32=0aBc\r\n", b"pcrc32"),
            Some(0xabc)
        );
        assert_eq!(crc_field(b"end size=5 pcrc32=0aBc\r\n", b"crc32"), None);
        assert_eq!(
            name_crc(line),
            Some(crate::vendor::yenc::crc32(0, b"a size=9 b.bin"))
        );
        assert_eq!(name_crc(b"=ybegin line=128 size=15\r\n"), None);
    }
}
