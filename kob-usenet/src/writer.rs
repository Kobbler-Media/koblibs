mod map;
mod pieces;

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use nix::fcntl::{FallocateFlags, fallocate};

use crate::jobs::fetch::{Fetcher, Outcome};
use crate::space::SpaceGate;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::{self, Lane};
use map::{Mark, SegmentMap};
use pieces::Piece;

const TICK: Duration = Duration::from_millis(50);
const FAILURES: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(500);
const SYNC_EVERY: Duration = Duration::from_secs(5);
const SYNC_QUIET: Duration = Duration::from_secs(1);

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
    pub gate: &'a SpaceGate,
    pub window: usize,
    pub tail: usize,
    pub straggler_after: Duration,
}

struct Output {
    file: Arc<File>,
    pieces: Vec<Piece>,
    map: SegmentMap,
}

enum Landed {
    Written,
    Damaged,
    Failed(nntp::Error),
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
        let mut outputs = targets.iter().map(open).collect::<io::Result<Vec<_>>>()?;
        let mut tally = Tally::default();
        let mut queue = VecDeque::new();
        for (o, output) in outputs.iter_mut().enumerate() {
            for (p, piece) in output.pieces.iter().enumerate() {
                tally.bytes_total += piece.len();
                tally.segments_total += 1;
                match output.map.get(p) {
                    Mark::Done => {
                        tally.bytes_done += piece.len();
                        tally.segments_done += 1;
                    }
                    Mark::Damaged => {}
                    Mark::Pending if piece.id.is_empty() => output.map.set(p, Mark::Damaged)?,
                    Mark::Pending => queue.push_back((o, p)),
                }
            }
        }
        progress(tally);
        let dest = targets
            .first()
            .and_then(|t| t.file.parent())
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let mut flying: HashMap<(usize, usize), Flight> = HashMap::new();
        let mut inflight = FuturesUnordered::new();
        let (mut latency, mut answered) = (Duration::ZERO, 0u32);
        let mut synced = Instant::now();
        let mut written = Vec::new();
        let mut last_written = Instant::now();
        let attempt = |key: (usize, usize), lane: Lane, outputs: &[Output], delay: Duration| {
            let output = &outputs[key.0];
            let piece = output.pieces[key.1].clone();
            let file = Arc::clone(&output.file);
            let fetcher = self.fetcher.clone();
            async move {
                tokio::time::sleep(delay).await;
                let started = Instant::now();
                let landed = land(&fetcher, lane, &piece, file).await;
                (key, landed, started.elapsed())
            }
        };
        while !queue.is_empty() || !flying.is_empty() {
            while flying.len() < self.window.max(1)
                && let Some(key) = queue.pop_front()
            {
                self.gate.wait(&dest).await;
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
                            written.push(key);
                            last_written = Instant::now();
                            tally.bytes_done += outputs[key.0].pieces[key.1].len();
                            tally.segments_done += 1;
                            latency += took;
                            answered += 1;
                            progress(tally);
                        }
                        _ if flight.pending > 0 => {}
                        Landed::Damaged => {
                            flying.remove(&key);
                            outputs[key.0].map.set(key.1, Mark::Damaged)?;
                        }
                        Landed::Failed(error) => {
                            flight.failures += 1;
                            if flight.failures >= FAILURES {
                                commit(&mut outputs, &mut written)?;
                                return Err(WriteError::Providers(error));
                            }
                            flight.pending = 1;
                            flight.since = None;
                            let delay = BACKOFF * flight.failures;
                            inflight.push(attempt(key, self.fetcher.lane, &outputs, delay));
                        }
                    }
                }
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
            let quiet = !written.is_empty() && last_written.elapsed() > SYNC_QUIET;
            if quiet || synced.elapsed() > SYNC_EVERY {
                commit(&mut outputs, &mut written)?;
                synced = Instant::now();
            }
        }
        commit(&mut outputs, &mut written)?;
        Ok(outputs.iter().map(damaged).collect())
    }
}

async fn land(fetcher: &Fetcher, lane: Lane, piece: &Piece, file: Arc<File>) -> Landed {
    let body = match fetcher.once(&piece.id, lane, piece.expectation()).await {
        Ok(Outcome::Body(body)) => body,
        Ok(Outcome::Missing | Outcome::Corrupt) => return Landed::Damaged,
        Err(error) => return Landed::Failed(error),
    };
    if !piece.fits(body.offset, body.data.len()) {
        return Landed::Damaged;
    }
    let data = body.data.slice(piece.src.clone());
    let at = piece.dest;
    match tokio::task::spawn_blocking(move || file.write_all_at(&data, at)).await {
        Ok(Ok(())) => Landed::Written,
        Ok(Err(error)) => Landed::Failed(nntp::Error::Io(error)),
        Err(error) => Landed::Failed(nntp::Error::Io(io::Error::other(error))),
    }
}

fn open(target: &Target<'_>) -> io::Result<Output> {
    if let Some(dir) = target.file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&target.file)?;
    let size = target.meta.size();
    let len = file.metadata()?.len();
    if len != size {
        let reserved = len == 0
            && i64::try_from(size).is_ok_and(|size| {
                size > 0 && fallocate(&file, FallocateFlags::empty(), 0, size).is_ok()
            });
        if !reserved {
            file.set_len(size)?;
        }
    }
    let pieces = pieces::of(target.meta);
    let map = SegmentMap::open(&target.map, pieces.len())?;
    Ok(Output {
        file: Arc::new(file),
        pieces,
        map,
    })
}

fn commit(outputs: &mut [Output], landed: &mut Vec<(usize, usize)>) -> io::Result<()> {
    for output in outputs.iter() {
        output.file.sync_data()?;
    }
    for (o, p) in landed.drain(..) {
        outputs[o].map.set(p, Mark::Done)?;
    }
    for output in outputs.iter() {
        output.map.sync()?;
    }
    Ok(())
}

fn damaged(output: &Output) -> Vec<Range<u64>> {
    output
        .pieces
        .iter()
        .enumerate()
        .filter(|&(p, _)| output.map.get(p) == Mark::Damaged)
        .map(|(_, piece)| piece.dest..piece.dest + piece.len())
        .collect()
}
