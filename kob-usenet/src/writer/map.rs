use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Pending = 0,
    Done = 1,
    Damaged = 2,
}

pub struct SegmentMap {
    file: File,
    marks: Vec<Mark>,
}

impl SegmentMap {
    pub fn open(path: &Path, len: usize) -> io::Result<Self> {
        let saved = std::fs::read(path).unwrap_or_default();
        let marks = if saved.len() == len {
            saved
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
        if saved.len() != len {
            file.set_len(0)?;
            file.set_len(len as u64)?;
        }
        Ok(Self { file, marks })
    }

    pub fn get(&self, index: usize) -> Mark {
        self.marks[index]
    }

    pub fn set(&mut self, index: usize, mark: Mark) -> io::Result<()> {
        self.marks[index] = mark;
        self.file.write_all_at(&[mark as u8], index as u64)
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }
}
