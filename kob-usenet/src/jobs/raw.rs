use std::collections::HashSet;

use futures_util::{StreamExt, stream};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::{Fetcher, Outcome};
use crate::jobs::store::{Plan, Planned};
use crate::repair::recovery;
use crate::vendor::core::{FileMeta, NzbFile, Par2Proof, Segment, Segments};
use crate::vendor::par2::{self, hash_16k};
use crate::vendor::queue::names::{self, Kind};
use crate::vendor::queue::nzb::{File, Nzb};

const FIRSTS: usize = 16;

struct Geometry {
    part: u64,
    size: u64,
}

pub async fn raw(fetcher: &Fetcher, nzb: &Nzb) -> Result<Plan, Failure> {
    let index = recovery::index(fetcher, nzb)
        .await
        .map_err(|e| Failure::providers(&e))?;
    let described = index.map(|r| r.index());
    let slice_size = described.as_ref().map_or(0, |i| i.slice_size);
    let described: Vec<&par2::File> = described.iter().flat_map(|i| &i.files).collect();
    let data: Vec<&File> = nzb
        .files
        .iter()
        .filter(|f| names::classify(f.name()) != Kind::Par2)
        .collect();
    let firsts = bodies(fetcher, data.iter().map(|f| f.segments[0].id.clone())).await?;
    let mut claimed = HashSet::new();
    let mut found: Vec<Option<(Geometry, Option<&par2::File>)>> = Vec::new();
    for (file, first) in data.iter().zip(&firsts) {
        let Some(first) = first else {
            found.push(None);
            continue;
        };
        if first.offset != 0 {
            return Err(uneven(file).into());
        }
        let start = &first.data[..first.data.len().min(16 << 10)];
        let matched = described
            .iter()
            .copied()
            .find(|f| f.hash_16k == hash_16k(start) && f.length == first.file_size);
        claimed.extend(matched.map(|f| f.id));
        let geometry = Geometry {
            part: first.data.len() as u64,
            size: first.file_size,
        };
        found.push(Some((geometry, matched)));
    }
    let seconds = bodies(
        fetcher,
        data.iter()
            .zip(&firsts)
            .filter(|(f, first)| first.is_none() && f.segments.len() > 1)
            .map(|(f, _)| f.segments[1].id.clone()),
    )
    .await?;
    let mut seconds = seconds.into_iter();
    for ((file, first), slot) in data.iter().zip(&firsts).zip(&mut found) {
        if first.is_some() {
            continue;
        }
        let second = if file.segments.len() > 1 {
            seconds.next().flatten()
        } else {
            None
        };
        let headless = rebuildable(file, second.as_ref(), &described, &claimed).ok_or_else(|| {
            Failure::Content(format!(
                "{}: its first article is missing on every provider and no PAR2 file identifies the volume to rebuild it",
                file.name()
            ))
        })?;
        claimed.insert(headless.1.id);
        *slot = Some((headless.0, Some(headless.1)));
    }
    let mut taken = HashSet::new();
    let mut outputs = Vec::new();
    for (file, slot) in data.into_iter().zip(found) {
        let (geometry, matched) = slot.expect("every volume was described above");
        let segments = uniform(file, &geometry)?;
        let name = matched.map_or_else(|| file.name().to_owned(), |f| f.name.clone());
        outputs.push(Planned {
            path: unique(
                &mut taken,
                names::sanitize(&name).unwrap_or_else(|| "file".to_owned()),
            ),
            meta: FileMeta::Nzb(NzbFile {
                segments,
                proof: matched.and_then(|f| proof(slice_size, f)),
            }),
            crc32: None,
            member: false,
        });
    }
    Ok(Plan {
        raw: true,
        probed: false,
        outputs,
    })
}

async fn bodies(
    fetcher: &Fetcher,
    ids: impl Iterator<Item = String>,
) -> Result<Vec<Option<crate::vendor::nntp::Body>>, Failure> {
    let requests: Vec<_> = ids
        .map(|id| {
            let fetcher = fetcher.clone();
            async move { fetcher.body(&id).await }
        })
        .collect();
    let answers: Vec<_> = stream::iter(requests).buffered(FIRSTS).collect().await;
    answers
        .into_iter()
        .map(|answer| match answer.map_err(|e| Failure::providers(&e))? {
            Outcome::Body(body) => Ok(Some(body)),
            Outcome::Missing | Outcome::Corrupt => Ok(None),
        })
        .collect()
}

fn rebuildable<'a>(
    file: &File,
    second: Option<&crate::vendor::nntp::Body>,
    described: &[&'a par2::File],
    claimed: &HashSet<par2::Id>,
) -> Option<(Geometry, &'a par2::File)> {
    let free = || {
        described
            .iter()
            .copied()
            .filter(|f| !claimed.contains(&f.id))
    };
    let by_name = free().find(|f| f.name == file.name());
    let Some(second) = second else {
        let only = by_name.filter(|_| file.segments.len() == 1)?;
        return Some((
            Geometry {
                part: only.length,
                size: only.length,
            },
            only,
        ));
    };
    let sized = |f: &&par2::File| f.length == second.file_size;
    let matched = by_name.filter(sized).or_else(|| {
        let mut candidates = free().filter(sized);
        let one = candidates.next()?;
        candidates.next().is_none().then_some(one)
    })?;
    Some((
        Geometry {
            part: second.offset,
            size: second.file_size,
        },
        matched,
    ))
}

fn uniform(file: &File, geometry: &Geometry) -> Result<Segments, String> {
    let count = file.segments.len() as u64;
    let Geometry { part, size } = *geometry;
    let last = size
        .checked_sub(part * (count - 1))
        .filter(|&last| last > 0 && last <= part)
        .ok_or_else(|| uneven(file))?;
    Ok(Segments {
        items: file
            .segments
            .iter()
            .zip(1..)
            .map(|(s, n)| Segment {
                id: s.id.clone(),
                size: u32::try_from(if n == count { last } else { part }).unwrap_or(u32::MAX),
                fallbacks: Vec::new(),
            })
            .collect(),
        sizes_trusted: true,
        ..Segments::default()
    })
}

fn uneven(file: &File) -> String {
    format!(
        "{}: its articles are not split evenly, which whole-volume downloads need",
        file.name()
    )
}

fn proof(slice_size: u64, file: &par2::File) -> Option<Par2Proof> {
    (!file.slices.is_empty()).then_some(Par2Proof {
        set_id: None,
        file_length: file.length,
        slice_size: u32::try_from(slice_size).ok()?,
        slice_md5: file.slices.iter().map(|s| s.md5).collect(),
        slice_crc32: file.slices.iter().map(|s| s.crc32).collect(),
        file_id: file.id,
        file_md5: file.hash,
        md5_16k: file.hash_16k,
    })
}

fn unique(taken: &mut HashSet<String>, name: String) -> String {
    let mut candidate = name.clone();
    let mut n = 1;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = format!("{name}.{n}");
        n += 1;
    }
    candidate
}
