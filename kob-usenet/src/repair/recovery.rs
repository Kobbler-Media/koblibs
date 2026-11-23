use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use futures_util::{StreamExt, stream};
use regex::Regex;

use crate::jobs::fetch::{Fetcher, First, Outcome};
use crate::repair::spool::{HEADER, MAGIC, RECOVERY, Spool};
use crate::vendor::nntp::{self, BodyExpectation};
use crate::vendor::par2::{Id, Reader};
use crate::vendor::queue::names::{self, Kind};
use crate::vendor::queue::nzb::{File, Nzb};

const PACKET_OVERHEAD: u64 = 68;
const YENC_OVERHEAD_PERCENT: u64 = 103;
const PARALLEL: usize = 8;
const DISCOVER: usize = 16;
const KNOWN: [&str; 26] = [
    "mkv", "mp4", "avi", "m4v", "mov", "ts", "m2ts", "wmv", "mpg", "webm", "srt", "sub", "idx",
    "ass", "nfo", "sfv", "txt", "jpg", "jpeg", "png", "nzb", "flac", "mp3", "m4a", "epub", "iso",
];

static VOLUME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.vol\d+[+-](\d+)\.par2$").expect("static regex"));

#[derive(Clone, Debug)]
struct Par2File {
    index: usize,
    named: Option<usize>,
    content: Option<Content>,
}

#[derive(Clone, Copy, Debug)]
struct Content {
    file_size: u64,
    packet: Option<u64>,
    whole: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Par2s {
    files: Vec<Par2File>,
}

pub struct Volume<'a> {
    pub blocks: usize,
    index: usize,
    stem: String,
    file: &'a File,
}

#[derive(Default)]
pub struct Collected {
    spool: Spool,
    cursors: HashMap<usize, usize>,
    current: Option<usize>,
    stems: HashMap<String, HashSet<Id>>,
}

impl Par2s {
    pub async fn discover(fetcher: &Fetcher, nzb: &Nzb) -> Result<Self, nntp::Error> {
        let probes: Vec<_> = nzb
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.segments.is_empty() && worth_reading(f))
            .map(|(index, file)| {
                let fetcher = fetcher.clone();
                let expected = BodyExpectation {
                    part: Some(1),
                    total: u32::try_from(file.segments.len()).ok(),
                    ..BodyExpectation::at(Some(0))
                };
                let id = file.segments[0].id.clone();
                async move { (index, fetcher.first(&id, expected).await) }
            })
            .collect();
        let read: Vec<_> = stream::iter(probes).buffered(DISCOVER).collect().await;
        let mut files: Vec<Par2File> = nzb
            .files
            .iter()
            .enumerate()
            .filter_map(|(index, f)| {
                volume_blocks(f.name()).map(|blocks| Par2File {
                    index,
                    named: Some(blocks),
                    content: None,
                })
            })
            .collect();
        for (index, answer) in read {
            let file = &nzb.files[index];
            let named = names::classify(file.name()) == Kind::Par2;
            let content = match answer? {
                First::Listed(body) | First::Unproven(body) if body.data.starts_with(MAGIC) => {
                    Some(Content {
                        file_size: body.file_size,
                        packet: first_recovery(&body.data),
                        whole: body.data.len() as u64 >= body.file_size,
                    })
                }
                _ if named => None,
                _ => continue,
            };
            files.push(Par2File {
                index,
                named: volume_blocks(file.name()),
                content,
            });
        }
        files.sort_by_key(|f| f.index);
        Ok(Self { files })
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn contains(&self, index: usize) -> bool {
        self.files.iter().any(|f| f.index == index)
    }

    pub fn volumes<'a>(&self, nzb: &'a Nzb, slice_size: usize) -> Vec<Volume<'a>> {
        self.files
            .iter()
            .filter_map(|p| {
                let file = &nzb.files[p.index];
                let blocks = p.blocks(file, slice_size as u64);
                (blocks > 0).then_some(Volume {
                    blocks,
                    index: p.index,
                    stem: stem(file.name()),
                    file,
                })
            })
            .collect()
    }

    pub fn recovery_blocks(&self, nzb: &Nzb, slice_size: usize) -> usize {
        self.volumes(nzb, slice_size).iter().map(|v| v.blocks).sum()
    }

    pub async fn index(
        &self,
        fetcher: &Fetcher,
        nzb: &Nzb,
        spool: Spool,
        enough: impl Fn(&Reader) -> bool,
    ) -> Result<Option<Collected>, nntp::Error> {
        let mut candidates: Vec<&Par2File> = self.files.iter().collect();
        candidates.sort_by_key(|p| {
            let file = &nzb.files[p.index];
            (p.blocks(file, 1) > 0, file.segments.len())
        });
        let mut collected = Collected {
            spool,
            ..Collected::default()
        };
        let mut read = HashSet::new();
        let mut satisfied = false;
        for candidate in candidates {
            let file = &nzb.files[candidate.index];
            let stem = stem(file.name());
            let critical = collected.spool.critical();
            if candidate.blocks(file, 1) > 0
                && !critical.sets().is_empty()
                && (read.contains(&stem) || satisfied)
            {
                continue;
            }
            let before: HashSet<Id> = critical.sets().into_iter().collect();
            collected
                .pull(fetcher, candidate.index, file, |_| usize::MAX, 1)
                .await?;
            let found: Vec<Id> = collected
                .spool
                .critical()
                .sets()
                .into_iter()
                .filter(|set| !before.contains(set))
                .collect();
            if !found.is_empty() {
                read.insert(stem.clone());
                collected.stems.entry(stem).or_default().extend(found);
                satisfied = enough(collected.spool.critical());
            }
        }
        Ok((!collected.spool.critical().sets().is_empty()).then_some(collected))
    }
}

impl Par2File {
    fn blocks(&self, file: &File, slice_size: u64) -> usize {
        if let Some(named) = self.named {
            return named;
        }
        let estimate = |size: u64, packet: u64| usize::try_from(size / packet).unwrap_or(0);
        match self.content {
            Some(Content {
                file_size,
                packet: Some(packet),
                ..
            }) => estimate(file_size, packet),
            Some(Content { whole: true, .. }) => 0,
            Some(Content { file_size, .. }) => estimate(file_size, slice_size + PACKET_OVERHEAD),
            None if file.name().to_ascii_lowercase().contains(".vol") => estimate(
                file.bytes() * 100 / YENC_OVERHEAD_PERCENT,
                slice_size + PACKET_OVERHEAD,
            ),
            None => 0,
        }
    }
}

impl Collected {
    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    pub async fn fetch(
        &mut self,
        fetcher: &Fetcher,
        volumes: &[Volume<'_>],
        set: Id,
        slice_size: usize,
        need: usize,
    ) -> Result<usize, nntp::Error> {
        let have = |spool: &Spool| spool.count(set, slice_size);
        let holds = |v: &Volume<'_>| self.stems.get(&v.stem).map(|sets| sets.contains(&set));
        let mut order: Vec<&Volume<'_>> =
            volumes.iter().filter(|v| holds(v) != Some(false)).collect();
        order.sort_by_key(|v| (holds(v) != Some(true), std::cmp::Reverse(v.blocks)));
        let known = order.iter().filter(|v| holds(v) == Some(true)).count();
        let window = if known > 0 { known } else { order.len() };
        let wanted = need.saturating_sub(have(&self.spool));
        if let Some(at) = order[..window].iter().rposition(|v| v.blocks >= wanted) {
            let best = order.remove(at);
            order.insert(0, best);
        }
        for volume in order {
            if have(&self.spool) >= need {
                break;
            }
            let packet = slice_size + HEADER + 4;
            let left = |s: &Spool| need.saturating_sub(have(s));
            self.pull(fetcher, volume.index, volume.file, left, packet)
                .await?;
        }
        Ok(have(&self.spool))
    }

    async fn pull(
        &mut self,
        fetcher: &Fetcher,
        index: usize,
        file: &File,
        left: impl Fn(&Spool) -> usize,
        packet: usize,
    ) -> Result<(), nntp::Error> {
        let start = self.cursors.get(&index).copied().unwrap_or(0);
        if start >= file.segments.len() || left(&self.spool) == 0 {
            return Ok(());
        }
        if self.current != Some(index) || start == 0 {
            self.spool.gap();
        }
        self.current = Some(index);
        let article = usize::try_from(
            file.bytes() * 100 / YENC_OVERHEAD_PERCENT / file.segments.len() as u64,
        )
        .unwrap_or(usize::MAX)
        .max(1);
        let mut at = start;
        while at < file.segments.len() {
            let wanted = left(&self.spool);
            if wanted == 0 {
                return Ok(());
            }
            let batch = wanted
                .saturating_mul(packet)
                .div_ceil(article)
                .clamp(1, PARALLEL)
                .min(file.segments.len() - at);
            let requests: Vec<_> = file.segments[at..at + batch]
                .iter()
                .map(|segment| {
                    let (fetcher, id) = (fetcher.clone(), segment.id.clone());
                    async move { fetcher.body(&id).await }
                })
                .collect();
            let answers: Vec<_> = stream::iter(requests).buffered(PARALLEL).collect().await;
            for answer in answers {
                match answer? {
                    Outcome::Body(body) => {
                        let mut spool = std::mem::take(&mut self.spool);
                        let (spool, fed) = tokio::task::spawn_blocking(move || {
                            let fed = spool.feed(&body.data);
                            (spool, fed)
                        })
                        .await
                        .map_err(|e| nntp::Error::Io(std::io::Error::other(e)))?;
                        self.spool = spool;
                        fed.map_err(nntp::Error::Io)?;
                    }
                    Outcome::Missing | Outcome::Corrupt => self.spool.gap(),
                }
                at += 1;
                self.cursors.insert(index, at);
            }
        }
        self.spool.gap();
        Ok(())
    }
}

fn worth_reading(file: &File) -> bool {
    let name = file.name();
    match names::classify(name) {
        Kind::Par2 => volume_blocks(name).is_none(),
        Kind::Plain => name
            .rsplit_once('.')
            .is_none_or(|(_, ext)| !KNOWN.contains(&ext.to_ascii_lowercase().as_str())),
        _ => false,
    }
}

fn stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let end = VOLUME
        .find(&lower)
        .map(|m| m.start())
        .or_else(|| lower.strip_suffix(".par2").map(str::len))
        .unwrap_or(lower.len());
    lower[..end].to_owned()
}

fn volume_blocks(name: &str) -> Option<usize> {
    VOLUME
        .captures(name)
        .and_then(|c| c[1].parse::<usize>().ok())
}

fn first_recovery(data: &[u8]) -> Option<u64> {
    let mut at = 0usize;
    while let Some(header) = data.get(at..at.checked_add(HEADER)?) {
        if &header[..8] != MAGIC {
            return None;
        }
        let len = u64::from_le_bytes(header[8..16].try_into().expect("8-byte field"));
        if &header[48..64] == RECOVERY {
            return Some(len);
        }
        if len < HEADER as u64 {
            return None;
        }
        at = at.checked_add(usize::try_from(len).ok()?)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(len: u64) -> Vec<u8> {
        [
            MAGIC.as_slice(),
            &len.to_le_bytes(),
            &[0; 32],
            b"PAR 2.0\0Main\0\0\0\0",
        ]
        .concat()
    }

    #[test]
    fn a_packet_length_that_cannot_advance_or_overflows_ends_the_walk() {
        for len in [0, 1, 63, u64::MAX, u64::MAX - 8] {
            assert_eq!(first_recovery(&header(len)), None, "{len}");
        }
    }
}
