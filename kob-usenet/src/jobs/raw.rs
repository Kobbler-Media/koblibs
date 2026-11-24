use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use futures_util::{StreamExt, stream};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::{Fetcher, First};
use crate::jobs::headless;
use crate::jobs::store::{Plan, Planned};
use crate::repair::recovery::Par2s;
use crate::repair::{self, Spool};
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
    let data: Vec<Cow<'_, File>> = nzb
        .files
        .iter()
        .enumerate()
        .filter(|&(i, f)| !par2.contains(i) && names::classify(f.name()) != Kind::Par2)
        .map(|(_, f)| numbered(f))
        .collect();
    let index = par2
        .index(fetcher, nzb, Spool::default(), |reader| {
            repair::described(reader).len() >= data.len()
        })
        .await
        .map_err(|e| Failure::providers(&e))?;
    let sized = index.map_or_else(Vec::new, |r| repair::described(r.spool().critical()));
    let slice_sizes: HashMap<par2::Id, u64> = sized.iter().map(|(f, s)| (f.id, *s)).collect();
    let described: Vec<&par2::File> = sized.iter().map(|(f, _)| f).collect();
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
    if let Some((file, _)) = data
        .iter()
        .zip(&firsts)
        .find(|(_, first)| first.as_ref().is_some_and(|b| b.offset != 0))
    {
        return Err(Failure::Content(uneven(file)));
    }
    let (matches, mut claimed) = claim(&data, &firsts, &described);
    let displaced: Vec<bool> = firsts
        .iter()
        .zip(&matches)
        .map(|(first, matched)| {
            matched.is_none()
                && first
                    .as_ref()
                    .is_some_and(|b| described.iter().any(|f| describes(f, b)))
        })
        .collect();
    let mut found: Vec<Option<(Geometry, Option<&par2::File>)>> = firsts
        .iter()
        .zip(&matches)
        .zip(&displaced)
        .map(|((first, &matched), &displaced)| {
            first.as_ref().filter(|_| !displaced).map(|first| {
                let geometry = Geometry {
                    part: first.data.len() as u64,
                    size: first.file_size,
                };
                (geometry, matched)
            })
        })
        .collect();
    for ((file, first), slot) in data.iter().zip(&firsts).zip(&mut found) {
        if slot.is_some() {
            continue;
        }
        let later = headless::later(fetcher, file).await?;
        let identified = headless::identify(file, later.as_ref(), &described, &claimed);
        *slot = Some(match (identified, first) {
            (Some((geometry, matched)), _) => {
                claimed.insert(matched.id);
                (geometry, Some(matched))
            }
            (None, Some(first)) => (
                Geometry {
                    part: first.data.len() as u64,
                    size: first.file_size,
                },
                None,
            ),
            (None, None) => {
                return Err(Failure::Content(format!(
                    "{}: its first article is missing on every provider and no PAR2 file identifies the volume to rebuild it",
                    file.name()
                )));
            }
        });
    }
    let mut taken = HashSet::new();
    let mut outputs = Vec::new();
    for (((file, slot), first), displaced) in
        data.into_iter().zip(found).zip(&firsts).zip(displaced)
    {
        let (geometry, matched) = slot.expect("every volume was described above");
        let file = match (matched, first.as_ref().filter(|_| !displaced)) {
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
                proof: matched.and_then(|f| proof(slice_sizes.get(&f.id).copied()?, f)),
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

// A file whose NZB name is the described name claims it first, so an article reposted under
// another file cannot take that entry from its real owner.
fn claim<'a>(
    data: &[Cow<'_, File>],
    firsts: &[Option<Body>],
    described: &[&'a par2::File],
) -> (Vec<Option<&'a par2::File>>, HashSet<par2::Id>) {
    let mut matches = vec![None; data.len()];
    let mut claimed = HashSet::new();
    for by_name in [true, false] {
        for (at, (file, first)) in data.iter().zip(firsts).enumerate() {
            let Some(first) = first.as_ref().filter(|_| matches[at].is_none()) else {
                continue;
            };
            let found = described.iter().copied().find(|f| {
                !claimed.contains(&f.id)
                    && (!by_name || f.name == file.name())
                    && describes(f, first)
            });
            if let Some(found) = found {
                claimed.insert(found.id);
                matches[at] = Some(found);
            }
        }
    }
    (matches, claimed)
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
    if geometry.part == 0 {
        return file;
    }
    let parts = geometry.size.div_ceil(geometry.part);
    let listed = file.segments.len() as u64;
    if listed == parts || first.total.map(u64::from) != Some(parts) || parts > 2 * listed {
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
    let last = part
        .checked_mul(count - 1)
        .and_then(|whole| size.checked_sub(whole))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(parts: usize) -> File {
        File {
            subject: "\"a.bin\" yEnc (1/1)".to_owned(),
            date: None,
            segments: (0..parts)
                .map(|n| nzb::Segment {
                    id: format!("{n}@x"),
                    bytes: 100,
                    number: n as i64 + 1,
                })
                .collect(),
            gap: false,
        }
    }

    fn first(total: u32) -> Body {
        Body {
            data: bytes::Bytes::from_static(&[1; 4]),
            offset: 0,
            file_size: 0,
            part: Some(1),
            total: Some(total),
            name_crc: None,
        }
    }

    #[test]
    fn a_claimed_part_count_far_past_the_listing_adds_no_segments() {
        let file = listed(3);
        let geometry = Geometry {
            part: 1,
            size: u64::from(u32::MAX),
        };
        let posted = as_posted(Cow::Borrowed(&file), &first(u32::MAX), &geometry);
        assert_eq!(posted.segments.len(), 3);
    }

    #[test]
    fn an_empty_first_article_has_no_part_count() {
        let file = listed(3);
        let geometry = Geometry { part: 0, size: 9 };
        let posted = as_posted(Cow::Borrowed(&file), &first(3), &geometry);
        assert_eq!(posted.segments.len(), 3);
    }

    #[test]
    fn a_part_size_that_overflows_the_file_is_uneven() {
        let geometry = Geometry {
            part: u64::MAX / 2,
            size: 9,
        };
        assert!(uniform(&listed(4), &geometry).is_err());
    }
}
