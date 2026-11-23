use std::borrow::Cow;
use std::collections::HashSet;

use futures_util::{StreamExt, stream};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::{Fetcher, First};
use crate::jobs::headless;
use crate::jobs::store::{Plan, Planned};
use crate::repair::Spool;
use crate::repair::recovery::Par2s;
use crate::vendor::core::{FileMeta, NzbFile, Par2Proof, Segment, Segments};
use crate::vendor::nntp::{Body, BodyExpectation};
use crate::vendor::par2::{self, hash_16k};
use crate::vendor::queue::names::{self, Kind};
use crate::vendor::queue::nzb::{self, File, Nzb};

const FIRSTS: usize = 16;

pub struct Geometry {
    pub part: u64,
    pub size: u64,
}

pub async fn raw(fetcher: &Fetcher, nzb: &Nzb, par2: &Par2s) -> Result<Plan, Failure> {
    let index = par2
        .index(fetcher, nzb, Spool::default())
        .await
        .map_err(|e| Failure::providers(&e))?;
    let described = index.map(|r| r.spool().critical().index());
    let slice_size = described.as_ref().map_or(0, |i| i.slice_size);
    let described: Vec<&par2::File> = described.iter().flat_map(|i| &i.files).collect();
    let data: Vec<Cow<'_, File>> = nzb
        .files
        .iter()
        .enumerate()
        .filter(|&(i, f)| !par2.contains(i) && names::classify(f.name()) != Kind::Par2)
        .map(|(_, f)| numbered(f))
        .collect();
    let firsts = bodies(
        fetcher,
        data.iter().map(|f| {
            let expected = BodyExpectation {
                part: Some(1),
                total: u32::try_from(f.segments.len()).ok(),
                ..BodyExpectation::at(Some(0))
            };
            (f.segments[0].id.clone(), expected)
        }),
        &described,
    )
    .await?;
    let mut claimed = HashSet::new();
    let mut found: Vec<Option<(Geometry, Option<&par2::File>)>> = Vec::new();
    for (file, first) in data.iter().zip(&firsts) {
        let Some(first) = first else {
            found.push(None);
            continue;
        };
        if first.offset != 0 {
            return Err(Failure::Content(uneven(file)));
        }
        let matched = described.iter().copied().find(|f| describes(f, first));
        claimed.extend(matched.map(|f| f.id));
        let geometry = Geometry {
            part: first.data.len() as u64,
            size: first.file_size,
        };
        found.push(Some((geometry, matched)));
    }
    for ((file, first), slot) in data.iter().zip(&firsts).zip(&mut found) {
        if first.is_some() {
            continue;
        }
        let later = headless::later(fetcher, file).await?;
        let (geometry, matched) = headless::identify(file, later.as_ref(), &described, &claimed)
            .ok_or_else(|| {
                Failure::Content(format!(
                    "{}: its first article is missing on every provider and no PAR2 file identifies the volume to rebuild it",
                    file.name()
                ))
            })?;
        claimed.insert(matched.id);
        *slot = Some((geometry, Some(matched)));
    }
    let mut taken = HashSet::new();
    let mut outputs = Vec::new();
    for ((file, slot), first) in data.into_iter().zip(found).zip(&firsts) {
        let (geometry, matched) = slot.expect("every volume was described above");
        let file = match (matched, first) {
            (Some(_), Some(first)) => as_posted(file, first, &geometry),
            _ => file,
        };
        let segments = uniform(&file, &geometry).map_err(Failure::Content)?;
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
    ids: impl Iterator<Item = (String, BodyExpectation)>,
    described: &[&par2::File],
) -> Result<Vec<Option<Body>>, Failure> {
    let requests: Vec<_> = ids
        .map(|(id, expected)| {
            let fetcher = fetcher.clone();
            async move { fetcher.first(&id, expected).await }
        })
        .collect();
    let answers: Vec<_> = stream::iter(requests).buffered(FIRSTS).collect().await;
    answers
        .into_iter()
        .map(|answer| match answer.map_err(|e| Failure::providers(&e))? {
            First::Listed(body) => Ok(Some(body)),
            First::Unproven(body) if described.iter().any(|f| describes(f, &body)) => {
                Ok(Some(body))
            }
            First::Unproven(_) | First::Missing | First::Corrupt => Ok(None),
        })
        .collect()
}

fn describes(file: &par2::File, first: &Body) -> bool {
    let start = &first.data[..first.data.len().min(16 << 10)];
    file.hash_16k == hash_16k(start) && file.length == first.file_size
}

/// A PAR2-proven volume listed with more or fewer entries than its evenly split post:
/// entries past the post are dropped and parts the NZB lacks are left for PAR2.
fn as_posted<'a>(file: Cow<'a, File>, first: &Body, geometry: &Geometry) -> Cow<'a, File> {
    let parts = geometry.size.div_ceil(geometry.part);
    let listed = file.segments.len() as u64;
    if listed == parts || first.total.map(u64::from) != Some(parts) {
        return file;
    }
    let mut file = file.into_owned();
    let parts = usize::try_from(parts).unwrap_or(usize::MAX);
    let bytes = file.segments.iter().map(|s| s.bytes).max().unwrap_or(0);
    let base = file.segments[0].number;
    file.segments.truncate(parts);
    while file.segments.len() < parts {
        let number = base.saturating_add(file.segments.len() as i64);
        file.segments.push(unlisted(number, bytes));
    }
    Cow::Owned(file)
}

fn unlisted(number: i64, bytes: u32) -> nzb::Segment {
    nzb::Segment {
        id: String::new(),
        bytes,
        number,
    }
}

/// The file's segments placed by part number. A part the NZB does not list gets an empty
/// id: no provider is asked for it, and the writer leaves it damaged for PAR2 to rebuild.
fn numbered(file: &File) -> Cow<'_, File> {
    let (Some(first), Some(last)) = (file.segments.first(), file.segments.last()) else {
        return Cow::Borrowed(file);
    };
    let base = first.number.min(1);
    let span = last
        .number
        .checked_sub(base)
        .and_then(|span| usize::try_from(span).ok())
        .and_then(|span| span.checked_add(1))
        .filter(|&span| span <= 2 * file.segments.len());
    let Some(span) = span.filter(|_| file.gap) else {
        return Cow::Borrowed(file);
    };
    let typical = file.segments.iter().map(|s| s.bytes).max().unwrap_or(0);
    let mut segments: Vec<nzb::Segment> = Vec::with_capacity(span);
    for segment in &file.segments {
        while base + (segments.len() as i64) < segment.number {
            segments.push(unlisted(base + segments.len() as i64, typical));
        }
        segments.push(segment.clone());
    }
    Cow::Owned(File {
        subject: file.subject.clone(),
        date: file.date,
        segments,
        gap: false,
    })
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
