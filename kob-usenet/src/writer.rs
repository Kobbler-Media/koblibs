mod ledger;
mod map;
mod pieces;

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use nix::fcntl::{FallocateFlags, fallocate};

use crate::disk::JobDisk;
use crate::jobs::fetch::{Fetcher, Outcome};
use crate::space::SpaceGate;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::{self, Lane};
use ledger::Ledger;
use map::{Identity, Mark, SegmentMap};
use pieces::Piece;

const TICK: Duration = Duration::from_millis(50);
const FAILURES: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(500);

pub struct Target<'a> {
    pub file: PathBuf,
    pub meta: &'a FileMeta,
    pub map: PathBuf,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Tally {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub segments_done: u64,
    pub segments_total: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("writing to disk: {0}")]
    Io(#[from] io::Error),
    #[error("providers unavailable: {0}")]
    Providers(nntp::Error),
}

pub struct Writer<'a> {
    pub fetcher: &'a Fetcher,
    pub gate: &'a Arc<SpaceGate>,
    pub disk: &'a Arc<dyn JobDisk>,
    pub window: usize,
    pub tail: usize,
    pub straggler_after: Duration,
    pub numbered: bool,
}

struct Output {
    file: Arc<File>,
    pieces: Vec<Piece>,
    reserved: bool,
}

#[derive(Debug)]
enum Landed {
    Written,
    Damaged,
    Failed(nntp::Error),
    Disk(io::Error),
}

#[derive(Default)]
struct Flight {
    since: Option<Instant>,
    hedged: bool,
    pending: u32,
    failures: u32,
}

impl Writer<'_> {
    pub async fn write(
        &self,
        targets: &[Target<'_>],
        progress: &(dyn Fn(Tally) + Sync),
    ) -> Result<Vec<Vec<Range<u64>>>, WriteError> {
        let owned: Vec<(PathBuf, FileMeta, PathBuf)> = targets
            .iter()
            .map(|t| (t.file.clone(), t.meta.clone(), t.map.clone()))
            .collect();
        let (gate, numbered) = (Arc::clone(self.gate), self.numbered);
        let (outputs, mut maps): (Vec<Output>, Vec<SegmentMap>) =
            tokio::task::spawn_blocking(move || {
                owned
                    .iter()
                    .map(|(file, meta, map)| open(file, meta, map, &gate, numbered))
                    .collect::<io::Result<Vec<_>>>()
            })
            .await
            .map_err(io::Error::other)??
            .into_iter()
            .unzip();
        let mut tally = Tally::default();
        let mut credit = 0u64;
        let mut queue = VecDeque::new();
        for (o, (output, map)) in outputs.iter().zip(&mut maps).enumerate() {
            for (p, piece) in output.pieces.iter().enumerate() {
                tally.bytes_total += piece.len();
                tally.segments_total += 1;
                match map.get(p) {
                    Mark::Done => {
                        tally.bytes_done += piece.len();
                        tally.segments_done += 1;
                    }
                    Mark::Damaged => {}
                    Mark::Pending if piece.id.is_empty() => map.set(p, Mark::Damaged)?,
                    Mark::Pending => {
                        if output.reserved {
                            credit += piece.len();
                        }
                        queue.push_back((o, p));
                    }
                }
            }
        }
        progress(tally);
        let files = outputs.iter().map(|o| Arc::clone(&o.file)).collect();
        let mut ledger = Ledger::new(Arc::clone(self.disk), files, maps);
        let dest = targets
            .first()
            .and_then(|t| t.file.parent())
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let mut flying: HashMap<(usize, usize), Flight> = HashMap::new();
        let mut inflight = FuturesUnordered::new();
        let (mut latency, mut answered) = (Duration::ZERO, 0u32);
        let mut waiting = None;
        let attempt = |key: (usize, usize), lane: Lane, outputs: &[Output], delay: Duration| {
            let output = &outputs[key.0];
            let piece = output.pieces[key.1].clone();
            let file = Arc::clone(&output.file);
            let fetcher = self.fetcher.clone();
            let disk = Arc::clone(self.disk);
            async move {
                tokio::time::sleep(delay).await;
                let started = Instant::now();
                let landed = land(&fetcher, lane, &piece, file, disk).await;
                (key, landed, started.elapsed())
            }
        };
        while !queue.is_empty() || !flying.is_empty() {
            while flying.len() < self.window.max(1)
                && let Some(&key) = queue.front()
                && (outputs[key.0].reserved || {
                    let admitted =
                        self.gate
                            .admits(&dest, credit, outputs[key.0].pieces[key.1].len());
                    if admitted {
                        waiting = None;
                    } else if waiting.is_none() {
                        waiting = Some(self.gate.wait());
                    }
                    admitted
                })
            {
                queue.pop_front();
                flying.insert(
                    key,
                    Flight {
                        since: Some(Instant::now()),
                        pending: 1,
                        ..Flight::default()
                    },
                );
                inflight.push(attempt(key, self.fetcher.lane, &outputs, Duration::ZERO));
            }
            tokio::select! {
                Some((key, landed, took)) = inflight.next() => {
                    let Some(flight) = flying.get_mut(&key) else {
                        continue;
                    };
                    flight.pending -= 1;
                    match landed {
                        Landed::Written => {
                            flying.remove(&key);
                            let len = outputs[key.0].pieces[key.1].len();
                            ledger.written(key.0, key.1, len);
                            if outputs[key.0].reserved {
                                credit = credit.saturating_sub(len);
                            }
                            tally.bytes_done += len;
                            tally.segments_done += 1;
                            latency += took;
                            answered += 1;
                            progress(tally);
                        }
                        Landed::Disk(error) => {
                            ledger.drain().await?;
                            return Err(WriteError::Io(error));
                        }
                        _ if flight.pending > 0 => {}
                        Landed::Damaged => {
                            flying.remove(&key);
                            ledger.damaged(key.0, key.1);
                        }
                        Landed::Failed(error) => {
                            flight.failures += 1;
                            if flight.failures >= FAILURES {
                                ledger.drain().await?;
                                return Err(WriteError::Providers(error));
                            }
                            flight.pending = 1;
                            flight.since = None;
                            let delay = BACKOFF * flight.failures;
                            inflight.push(attempt(key, self.fetcher.lane, &outputs, delay));
                        }
                    }
                }
                result = ledger.finished(), if ledger.syncing() => ledger.settle(result)?,
                () = tokio::time::sleep(TICK) => {}
            }
            if queue.is_empty() && flying.len() < self.tail.max(1) {
                let threshold = self
                    .straggler_after
                    .max(latency.checked_div(answered).unwrap_or_default() * 3);
                for (&key, flight) in &mut flying {
                    if !flight.hedged && flight.since.is_some_and(|t| t.elapsed() > threshold) {
                        flight.hedged = true;
                        flight.pending += 1;
                        tracing::debug!(segment = %outputs[key.0].pieces[key.1].id, "re-issuing a straggler");
                        inflight.push(attempt(key, Lane::default(), &outputs, Duration::ZERO));
                    }
                }
            }
            ledger.sync_if_due();
        }
        ledger.drain().await?;
        Ok(outputs
            .iter()
            .enumerate()
            .map(|(o, output)| damaged(&ledger, o, output))
            .collect())
    }
}

async fn land(
    fetcher: &Fetcher,
    lane: Lane,
    piece: &Piece,
    file: Arc<File>,
    disk: Arc<dyn JobDisk>,
) -> Landed {
    let body = match fetcher.once(&piece.id, lane, piece.expectation()).await {
        Ok(Outcome::Body(body)) => body,
        Ok(Outcome::Missing | Outcome::Corrupt) => return Landed::Damaged,
        Err(error) => return Landed::Failed(error),
    };
    if !piece.fits(body.offset, body.data.len()) {
        return Landed::Damaged;
    }
    store(disk, file, body.data.slice(piece.src.clone()), piece.dest).await
}

async fn store(disk: Arc<dyn JobDisk>, file: Arc<File>, data: bytes::Bytes, at: u64) -> Landed {
    match tokio::task::spawn_blocking(move || disk.write_at(&file, &data, at)).await {
        Ok(Ok(())) => Landed::Written,
        Ok(Err(error)) => Landed::Disk(error),
        Err(error) => Landed::Disk(io::Error::other(error)),
    }
}

fn open(
    path: &Path,
    meta: &FileMeta,
    map_path: &Path,
    gate: &SpaceGate,
    numbered: bool,
) -> io::Result<(Output, SegmentMap)> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let existed = path.exists();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let size = meta.size();
    let fresh = !existed || file.metadata()?.len() != size;
    if fresh {
        reset(map_path, &file, |file| {
            file.set_len(0)?;
            if !reserve(file, dir, size, gate) {
                file.set_len(size)?;
            }
            Ok(())
        })?;
    }
    let stat = file.metadata()?;
    let identity = Identity {
        inode: stat.ino(),
        len: size,
    };
    let pieces = pieces::of(meta, numbered);
    let map = SegmentMap::open(map_path, pieces.len(), (!fresh).then_some(identity))?;
    map.stamp(identity)?;
    Ok((
        Output {
            file: Arc::new(file),
            pieces,
            reserved: stat.blocks().saturating_mul(512) >= size,
        },
        map,
    ))
}

// The map goes first: a crash mid-reset must not leave old Done marks trusted over zeros.
fn reset(
    map_path: &Path,
    file: &File,
    resize: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    SegmentMap::invalidate(map_path)?;
    resize(file)
}

fn reserve(file: &File, dir: &Path, size: u64, gate: &SpaceGate) -> bool {
    let Ok(len) = i64::try_from(size) else {
        return false;
    };
    if len == 0 || !gate.can_reserve(dir, size) {
        return false;
    }
    if fallocate(file, FallocateFlags::empty(), 0, len).is_ok() {
        return true;
    }
    let _ = file.set_len(0);
    false
}

fn damaged(ledger: &Ledger, o: usize, output: &Output) -> Vec<Range<u64>> {
    output
        .pieces
        .iter()
        .enumerate()
        .filter(|&(p, _)| ledger.is_damaged(o, p))
        .map(|(_, piece)| piece.dest..piece.dest + piece.len())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_while_resetting_a_truncated_file_leaves_no_done_marks() {
        let dir = tempfile::tempdir().unwrap();
        let (data, map_path) = (dir.path().join("data"), dir.path().join("map"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&data)
            .unwrap();
        file.set_len(4096).unwrap();
        let identity = Identity {
            inode: file.metadata().unwrap().ino(),
            len: 4096,
        };
        let mut map = SegmentMap::open(&map_path, 2, None).unwrap();
        map.set(0, Mark::Done).unwrap();
        map.stamp(identity).unwrap();
        drop(map);
        file.set_len(1).unwrap();

        let crashed = reset(&map_path, &file, |file| {
            file.set_len(0)?;
            file.set_len(4096)?;
            Err(io::Error::other("power cut"))
        });

        assert!(crashed.is_err());
        let reopened = SegmentMap::open(&map_path, 2, Some(identity)).unwrap();
        assert_eq!(reopened.get(0), Mark::Pending, "old marks over zeroed data");
    }

    #[tokio::test]
    async fn a_full_disk_is_a_disk_failure_not_a_provider_failure() {
        let full = File::options().write(true).open("/dev/full").unwrap();
        let disk: Arc<dyn JobDisk> = Arc::new(crate::disk::DirectDisk);
        let landed = store(
            disk,
            Arc::new(full),
            bytes::Bytes::from_static(&[1; 4096]),
            0,
        )
        .await;
        assert!(
            matches!(&landed, Landed::Disk(e) if e.kind() == io::ErrorKind::StorageFull),
            "{landed:?}"
        );
    }
}
