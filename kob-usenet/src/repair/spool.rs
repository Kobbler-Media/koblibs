use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use md5::{Digest, Md5};

use crate::vendor::par2::{Id, Reader};

pub const MAGIC: &[u8; 8] = b"PAR2\0PKT";
pub const RECOVERY: &[u8; 16] = b"PAR 2.0\0RecvSlic";
pub const HEADER: usize = 64;
const MAX_CRITICAL: usize = 16 << 20;
const MAX_RECOVERY: usize = 256 << 20;

struct Stored {
    set: Id,
    exponent: u32,
    at: u64,
    len: usize,
}

#[derive(Clone, Copy)]
pub struct Row {
    pub exponent: u32,
    at: u64,
}

#[derive(Default)]
pub struct Spool {
    file: Option<File>,
    end: u64,
    stored: Vec<Stored>,
    exponents: HashSet<(Id, u32, usize)>,
    carry: Vec<u8>,
    critical: Reader,
}

impl Spool {
    pub fn on_disk(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(".recovery-{}", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::fs::remove_file(&path)?;
        Ok(Self {
            file: Some(file),
            ..Self::default()
        })
    }

    pub const fn critical(&self) -> &Reader {
        &self.critical
    }

    #[cfg(test)]
    pub fn carried(&self) -> usize {
        self.carry.len()
    }

    pub fn feed(&mut self, data: &[u8]) -> io::Result<()> {
        self.carry.extend_from_slice(data);
        let mut at = 0;
        loop {
            let Some(start) = self.carry[at..]
                .windows(MAGIC.len())
                .position(|w| w == MAGIC)
                .map(|p| p + at)
            else {
                at = self.carry.len().saturating_sub(MAGIC.len() - 1).max(at);
                break;
            };
            at = start;
            let Some(header) = self.carry.get(at..at + HEADER) else {
                break;
            };
            let len = usize::try_from(u64::from_le_bytes(
                header[8..16].try_into().expect("8-byte field"),
            ))
            .unwrap_or(usize::MAX);
            let recovery = &header[48..64] == RECOVERY;
            let cap = if recovery { MAX_RECOVERY } else { MAX_CRITICAL };
            if len < HEADER || !len.is_multiple_of(4) || len > cap {
                at += 1;
                continue;
            }
            let Some(packet) = self.carry.get(at..at + len) else {
                break;
            };
            if <[u8; 16]>::from(Md5::digest(&packet[32..])) != packet[16..32] {
                at += 1;
                continue;
            }
            if recovery {
                let packet = packet.to_vec();
                self.store(&packet)?;
            } else {
                let packet = packet.to_vec();
                self.critical.push(&packet);
                self.critical.end_volume();
            }
            at += len;
        }
        self.carry.drain(..at);
        Ok(())
    }

    pub fn gap(&mut self) {
        self.carry.clear();
    }

    fn store(&mut self, packet: &[u8]) -> io::Result<()> {
        if packet.len() < HEADER + 4 {
            return Ok(());
        }
        let set: Id = packet[32..48].try_into().expect("16-byte field");
        let exponent = u32::from_le_bytes(packet[64..68].try_into().expect("4-byte field"));
        if !self.exponents.insert((set, exponent, packet.len())) {
            return Ok(());
        }
        let at = self.end;
        if let Some(file) = &self.file {
            file.write_all_at(packet, at)?;
        }
        self.end += packet.len() as u64;
        self.stored.push(Stored {
            set,
            exponent,
            at,
            len: packet.len(),
        });
        Ok(())
    }

    pub fn count(&self, set: Id, slice_size: usize) -> usize {
        self.matching(set, slice_size).count()
    }

    pub fn rows(&self, set: Id, slice_size: usize, count: usize) -> Vec<Row> {
        self.matching(set, slice_size)
            .take(count)
            .map(|s| Row {
                exponent: s.exponent,
                at: s.at,
            })
            .collect()
    }

    pub fn read(&self, row: Row, offset: usize, buf: &mut [u8]) -> io::Result<()> {
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| io::Error::other("recovery spool has no backing file"))?;
        file.read_exact_at(buf, row.at + (HEADER + 4 + offset) as u64)
    }

    fn matching(&self, set: Id, slice_size: usize) -> impl Iterator<Item = &Stored> {
        self.stored
            .iter()
            .filter(move |s| s.set == set && s.len == slice_size + HEADER + 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(set: [u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let rest = [set.as_slice(), kind, body].concat();
        let len = (32 + rest.len()) as u64;
        [
            MAGIC.as_slice(),
            &len.to_le_bytes(),
            &Md5::digest(&rest),
            &rest,
        ]
        .concat()
    }

    #[test]
    fn recovery_packets_go_to_disk_and_memory_holds_at_most_one_packet() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::on_disk(dir.path()).unwrap();
        let slice = 64 * 1024;
        let set = [7; 16];
        let mut volume = Vec::new();
        for exponent in 0..64u32 {
            let body = [exponent.to_le_bytes().to_vec(), vec![exponent as u8; slice]].concat();
            volume.extend(packet(set, RECOVERY, &body));
        }
        let mut peak = 0;
        for article in volume.chunks(16 * 1024) {
            spool.feed(article).unwrap();
            peak = peak.max(spool.carried());
        }
        assert_eq!(spool.count(set, slice), 64);
        assert!(peak <= slice + HEADER + 4, "carried {peak} bytes");
        let rows = spool.rows(set, slice, 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].exponent, 2);
        let mut stripe = [0; 8];
        spool.read(rows[2], 100, &mut stripe).unwrap();
        assert_eq!(stripe, [2; 8]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_gap_drops_the_packet_it_cut_and_keeps_the_rest() {
        let mut spool = Spool::default();
        let set = [1; 16];
        let one = packet(
            set,
            RECOVERY,
            &[0u32.to_le_bytes().as_slice(), &[1; 8]].concat(),
        );
        let two = packet(
            set,
            RECOVERY,
            &[1u32.to_le_bytes().as_slice(), &[2; 8]].concat(),
        );
        spool.feed(&one[..40]).unwrap();
        spool.gap();
        spool.feed(&two).unwrap();
        assert_eq!(spool.count(set, 8), 1);
    }
}
