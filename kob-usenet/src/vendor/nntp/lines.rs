//! Structural checks on a streaming yEnc body's raw lines, so a damaged copy stops
//! being served before its CRC can be checked at `=yend`.

use crate::vendor::nntp::Error;

pub(crate) const BROKEN: Error = Error::Yenc("article breaks yEnc line structure");

/// Raw data lines of one article, fed in the chunks the decoder consumed. Lines are
/// `line=` characters, or one more when an escape pair crosses that length; only a
/// line before `=yend` or the terminator may be shorter, by more than one.
pub(crate) struct Lines {
    line: Option<usize>,
    /// Raw bytes on the current line, the stuffed dot included.
    raw: usize,
    /// Characters on the current line, the stuffed dot excluded.
    col: usize,
    lead: u8,
    /// The last character opened an escape pair.
    escape: bool,
    cr: bool,
    /// The previous line was short, so this one must be a control line or the terminator.
    short: bool,
}

impl Lines {
    pub(crate) const fn new(line: Option<usize>) -> Self {
        Self {
            line,
            raw: 0,
            col: 0,
            lead: 0,
            escape: false,
            cr: false,
            short: false,
        }
    }

    /// After a `=y` control line, data resumes at a line start.
    pub(crate) const fn control(&mut self) {
        *self = Self::new(self.line);
    }

    pub(crate) fn scan(&mut self, raw: &[u8]) -> Result<(), Error> {
        for &byte in raw {
            if self.cr {
                self.cr = false;
                if byte != b'\n' {
                    return Err(BROKEN);
                }
                self.end_line()?;
                continue;
            }
            match byte {
                b'\r' => self.cr = true,
                b'\n' => self.end_line()?,
                0 => return Err(BROKEN),
                byte => self.char(byte)?,
            }
        }
        Ok(())
    }

    fn char(&mut self, byte: u8) -> Result<(), Error> {
        let raw = self.raw;
        self.raw += 1;
        if raw == 0 {
            self.lead = byte;
            if self.short && byte != b'=' && byte != b'.' {
                return Err(BROKEN);
            }
        } else if raw == 1 {
            if self.short && !(self.lead == b'=' && byte == b'y') {
                return Err(BROKEN);
            }
            if self.lead == b'.' && byte == b'.' {
                return Ok(());
            }
        }
        if let Some(line) = self.line
            && (self.col > line || (self.col == line && !self.escape))
        {
            return Err(BROKEN);
        }
        self.col += 1;
        self.escape = !self.escape && byte == b'=';
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), Error> {
        let terminator = self.raw == 1 && self.lead == b'.';
        if self.short && !terminator {
            return Err(BROKEN);
        }
        self.short = self.line.is_some_and(|line| self.col + 1 < line);
        (self.raw, self.col, self.escape) = (0, 0, false);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::yenc::encode;

    fn stuffed(data: &[u8], line: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut start = true;
        for &b in &encode(data, line) {
            if start && b == b'.' {
                out.push(b'.');
            }
            out.push(b);
            start = b == b'\n';
        }
        out.extend_from_slice(b"\r\n");
        out
    }

    #[test]
    fn real_encoder_output_passes_in_any_chunking() {
        let critical: Vec<u8> = [0u8, 10, 13, 19, 214, 224, 227, 4]
            .iter()
            .copied()
            .cycle()
            .take(5000)
            .collect();
        let mixed: Vec<u8> = (0..20_000u32).map(|i| (i * 7919 % 251) as u8).collect();
        for data in [critical, mixed] {
            for line in [128u16, 64, 3] {
                let wire = stuffed(&data, line);
                for chunk in [1, 7, 4096] {
                    let mut lines = Lines::new(Some(usize::from(line)));
                    for part in wire.chunks(chunk) {
                        lines.scan(part).unwrap();
                    }
                    lines.scan(b"=y").unwrap();
                    lines.control();
                    lines.scan(b".\r\n").unwrap();
                }
            }
        }
    }

    #[test]
    fn damage_is_found_at_the_byte_that_shows_it() {
        let wire = stuffed(&[7u8; 1000], 128);
        let cases: [(&str, fn(&mut Vec<u8>)); 5] = [
            ("nul", |w| w[300] = 0),
            ("bare cr", |w| w[300] = b'\r'),
            ("short line", |w| {
                w.drain(200..203);
            }),
            ("long line", |w| {
                w.splice(200..200, *b"xx");
            }),
            ("bare lf mid-line", |w| w[300] = b'\n'),
        ];
        for (case, damage) in cases {
            let mut wire = wire.clone();
            damage(&mut wire);
            let mut lines = Lines::new(Some(128));
            assert!(lines.scan(&wire).is_err(), "{case}");
        }
    }
}
