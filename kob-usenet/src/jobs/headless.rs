use std::collections::HashSet;

use crate::jobs::failure::Failure;
use crate::jobs::fetch::{Fetcher, Outcome};
use crate::jobs::raw::Geometry;
use crate::vendor::nntp::Body;
use crate::vendor::par2;
use crate::vendor::queue::nzb::File;

const LOOKAHEAD: usize = 8;
const YENC_OVERHEAD: u64 = 10;

pub struct Later {
    index: u64,
    body: Body,
}

pub async fn later(fetcher: &Fetcher, file: &File) -> Result<Option<Later>, Failure> {
    for (index, segment) in file.segments.iter().enumerate().skip(1).take(LOOKAHEAD) {
        match fetcher
            .body(&segment.id)
            .await
            .map_err(|e| Failure::providers(&e))?
        {
            Outcome::Body(body) => {
                return Ok(Some(Later {
                    index: index as u64,
                    body,
                }));
            }
            Outcome::Missing | Outcome::Corrupt => {}
        }
    }
    Ok(None)
}

pub fn identify<'a>(
    file: &File,
    later: Option<&Later>,
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
    let Some(Later { index, body }) = later else {
        let only = by_name.filter(|_| file.segments.len() == 1)?;
        return Some((
            Geometry {
                part: only.length,
                size: only.length,
            },
            only,
        ));
    };
    let part = (body.offset % index == 0)
        .then_some(body.offset / index)
        .filter(|&part| part > 0)?;
    let count = file.segments.len() as u64;
    let posted: u64 = file.segments.iter().map(|s| u64::from(s.bytes)).sum();
    let headed = |f: &&par2::File| f.length == body.file_size;
    // Posters that misstate `=ybegin size=` still post every byte; the encoded total bounds the
    // decoded length from above, and yEnc escaping and line breaks add only a few percent.
    let fits = |f: &&par2::File| {
        f.length.div_ceil(part) == count
            && f.length <= posted
            && f.length >= posted - posted / YENC_OVERHEAD
    };
    let matched = by_name
        .filter(|f| headed(f) || fits(f))
        .or_else(|| sole(free().filter(headed)))
        .or_else(|| sole(free().filter(fits)))?;
    Some((
        Geometry {
            part,
            size: matched.length,
        },
        matched,
    ))
}

fn sole<T>(mut candidates: impl Iterator<Item = T>) -> Option<T> {
    let one = candidates.next()?;
    candidates.next().is_none().then_some(one)
}
