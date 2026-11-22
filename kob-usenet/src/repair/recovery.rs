use std::sync::LazyLock;

use regex::Regex;

use crate::jobs::fetch::{Fetcher, Outcome};
use crate::vendor::nntp;
use crate::vendor::par2::{Id, Reader};
use crate::vendor::queue::names::{self, Kind};
use crate::vendor::queue::nzb::{File, Nzb};

const PACKET_OVERHEAD: u64 = 68;
const YENC_OVERHEAD_PERCENT: u64 = 103;

static VOLUME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.vol\d+[+-](\d+)\.par2$").expect("static regex"));

pub struct Volume<'a> {
    pub blocks: usize,
    file: &'a File,
}

pub fn par2_files(nzb: &Nzb) -> impl Iterator<Item = &File> {
    nzb.files
        .iter()
        .filter(|f| names::classify(f.name()) == Kind::Par2)
}

pub fn volumes(nzb: &Nzb, slice_size: usize) -> Vec<Volume<'_>> {
    par2_files(nzb)
        .filter_map(|file| {
            let named = VOLUME
                .captures(file.name())
                .and_then(|c| c[1].parse::<usize>().ok());
            let estimated = || {
                let decoded = file.bytes() * 100 / YENC_OVERHEAD_PERCENT;
                usize::try_from(decoded / (slice_size as u64 + PACKET_OVERHEAD)).unwrap_or(0)
            };
            let blocks = named.unwrap_or_else(|| {
                if VOLUME.is_match(file.name()) || file.name().to_ascii_lowercase().contains(".vol")
                {
                    estimated()
                } else {
                    0
                }
            });
            (blocks > 0).then_some(Volume { blocks, file })
        })
        .collect()
}

pub fn recovery_blocks(nzb: &Nzb, slice_size: usize) -> usize {
    volumes(nzb, slice_size).iter().map(|v| v.blocks).sum()
}

pub async fn index(fetcher: &Fetcher, nzb: &Nzb) -> Result<Option<Reader>, nntp::Error> {
    let mut candidates: Vec<&File> = par2_files(nzb).collect();
    candidates.sort_by_key(|f| (VOLUME.is_match(f.name()), f.segments.len()));
    let Some(file) = candidates.first() else {
        return Ok(None);
    };
    let mut reader = Reader::default();
    push(fetcher, &mut reader, file, |_| false).await?;
    Ok((!reader.sets().is_empty()).then_some(reader))
}

pub async fn fetch(
    fetcher: &Fetcher,
    reader: &mut Reader,
    volumes: &[Volume<'_>],
    set: Id,
    slice_size: usize,
    need: usize,
) -> Result<usize, nntp::Error> {
    let have = |reader: &Reader| {
        reader
            .recovery_slices(set, slice_size)
            .map_or(0, |r| r.len())
    };
    let mut order: Vec<&Volume<'_>> = volumes.iter().collect();
    order.sort_by_key(|v| std::cmp::Reverse(v.blocks));
    let wanted = need.saturating_sub(have(reader));
    if let Some(at) = order.iter().rposition(|v| v.blocks >= wanted) {
        let best = order.remove(at);
        order.insert(0, best);
    }
    for volume in order {
        if have(reader) >= need {
            break;
        }
        push(fetcher, reader, volume.file, |r| have(r) >= need).await?;
    }
    Ok(have(reader))
}

async fn push(
    fetcher: &Fetcher,
    reader: &mut Reader,
    file: &File,
    enough: impl Fn(&Reader) -> bool,
) -> Result<(), nntp::Error> {
    for segment in &file.segments {
        if enough(reader) {
            break;
        }
        match fetcher.body(&segment.id).await? {
            Outcome::Body(body) => reader.push(&body.data),
            Outcome::Missing | Outcome::Corrupt => reader.end_volume(),
        }
    }
    reader.end_volume();
    Ok(())
}
