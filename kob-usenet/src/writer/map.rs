use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

pub const HEADER: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Pending = 0,
    Done = 1,
    Damaged = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub inode: u64,
    pub len: u64,
}

impl Identity {
    fn bytes(self) -> [u8; HEADER] {
        let mut out = [0; HEADER];
        out[..8].copy_from_slice(&self.inode.to_le_bytes());
        out[8..].copy_from_slice(&self.len.to_le_bytes());
        out
    }
}

pub struct SegmentMap {
    file: File,
    marks: Vec<Mark>,
}

impl SegmentMap {
    pub fn open(path: &Path, len: usize, data: Option<Identity>) -> io::Result<Self> {
        let saved = std::fs::read(path).unwrap_or_default();
        let trusted = data.filter(|id| {
            saved.len() == HEADER + len && saved.get(..HEADER) == Some(id.bytes().as_slice())
        });
        let marks = if trusted.is_some() {
            saved[HEADER..]
                .iter()
                .map(|&b| match b {
                    1 => Mark::Done,
                    2 => Mark::Damaged,
                    _ => Mark::Pending,
                })
                .collect()
        } else {
            vec![Mark::Pending; len]
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(Self { file, marks })
    }

    pub fn stamp(&self, data: Identity) -> io::Result<()> {
        let mut bytes = data.bytes().to_vec();
        bytes.extend(self.marks.iter().map(|&m| m as u8));
        self.file.set_len(0)?;
        self.file.write_all_at(&bytes, 0)?;
        self.file.sync_data()
    }

    pub fn get(&self, index: usize) -> Mark {
        self.marks[index]
    }

    pub fn set(&mut self, index: usize, mark: Mark) -> io::Result<()> {
        self.marks[index] = mark;
        self.file
            .write_all_at(&[mark as u8], (HEADER + index) as u64)
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }
}
