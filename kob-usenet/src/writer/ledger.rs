use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::task::{JoinError, JoinHandle};

use super::map::{Mark, SegmentMap};
use crate::disk::JobDisk;

const SYNC_EVERY: Duration = Duration::from_secs(5);
const SYNC_QUIET: Duration = Duration::from_secs(1);
const SYNC_LATEST: Duration = Duration::from_secs(30);
const UNSYNCED_MOST: u64 = 1 << 30;

struct Books {
    maps: Vec<SegmentMap>,
    live: bool,
}

pub(super) struct Ledger {
    disk: Arc<dyn JobDisk>,
    files: Vec<Arc<File>>,
    books: Arc<Mutex<Books>>,
    marks: Vec<(usize, usize, Mark)>,
    unsynced: BTreeSet<usize>,
    unsynced_bytes: u64,
    running: Option<JoinHandle<io::Result<Duration>>>,
    next: Instant,
    last_landed: Instant,
}

fn lock(books: &Mutex<Books>) -> MutexGuard<'_, Books> {
    books.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Ledger {
    pub(super) fn new(
        disk: Arc<dyn JobDisk>,
        files: Vec<Arc<File>>,
        maps: Vec<SegmentMap>,
    ) -> Self {
        let now = Instant::now();
        Self {
            disk,
            files,
            books: Arc::new(Mutex::new(Books { maps, live: true })),
            marks: Vec::new(),
            unsynced: BTreeSet::new(),
            unsynced_bytes: 0,
            running: None,
            next: now + SYNC_EVERY,
            last_landed: now,
        }
    }

    pub(super) fn written(&mut self, output: usize, piece: usize, bytes: u64) {
        self.marks.push((output, piece, Mark::Done));
        self.unsynced.insert(output);
        self.unsynced_bytes += bytes;
        self.last_landed = Instant::now();
    }

    pub(super) fn damaged(&mut self, output: usize, piece: usize) {
        self.marks.push((output, piece, Mark::Damaged));
    }

    pub(super) fn syncing(&self) -> bool {
        self.running.is_some()
    }

    pub(super) async fn finished(&mut self) -> Result<io::Result<Duration>, JoinError> {
        match &mut self.running {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    }

    pub(super) fn settle(
        &mut self,
        result: Result<io::Result<Duration>, JoinError>,
    ) -> io::Result<()> {
        self.running = None;
        let took = result.map_err(io::Error::other)??;
        self.next = Instant::now() + took.clamp(SYNC_EVERY, SYNC_LATEST);
        Ok(())
    }

    pub(super) fn sync_if_due(&mut self) {
        if self.running.is_some() || self.marks.is_empty() {
            return;
        }
        let now = Instant::now();
        if now >= self.next
            || now.duration_since(self.last_landed) > SYNC_QUIET
            || self.unsynced_bytes >= UNSYNCED_MOST
        {
            self.start();
        }
    }

    fn start(&mut self) {
        let marks = std::mem::take(&mut self.marks);
        let files: Vec<Arc<File>> = std::mem::take(&mut self.unsynced)
            .into_iter()
            .map(|o| Arc::clone(&self.files[o]))
            .collect();
        self.unsynced_bytes = 0;
        let (disk, books) = (Arc::clone(&self.disk), Arc::clone(&self.books));
        self.running = Some(tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            for file in &files {
                disk.sync(file)?;
            }
            record(&books, &marks)?;
            Ok(started.elapsed())
        }));
    }

    pub(super) async fn drain(&mut self) -> io::Result<()> {
        if self.running.is_some() {
            let result = self.finished().await;
            self.settle(result)?;
        }
        if !self.marks.is_empty() {
            self.start();
            let result = self.finished().await;
            self.settle(result)?;
        }
        Ok(())
    }

    pub(super) fn is_damaged(&self, output: usize, piece: usize) -> bool {
        lock(&self.books).maps[output].get(piece) == Mark::Damaged
    }
}

fn record(books: &Mutex<Books>, marks: &[(usize, usize, Mark)]) -> io::Result<()> {
    let touched: BTreeSet<usize> = marks.iter().map(|&(o, _, _)| o).collect();
    let handles = {
        let mut books = lock(books);
        if !books.live {
            return Ok(());
        }
        for &(output, piece, mark) in marks {
            books.maps[output].set(piece, mark)?;
        }
        touched
            .iter()
            .map(|&o| books.maps[o].handle())
            .collect::<Vec<_>>()
    };
    handles.iter().try_for_each(|map| map.sync_data())
}

// A sync outliving a cancelled job must not mark segments in a map the job's next run owns.
impl Drop for Ledger {
    fn drop(&mut self) {
        lock(&self.books).live = false;
    }
}
