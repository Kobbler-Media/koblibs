use std::collections::HashSet;
use std::sync::Arc;

use futures_util::{StreamExt, stream};

use crate::ImportMode;
use crate::jobs::fetch::{Fetcher, Outcome};
use crate::jobs::store::{Plan, Planned};
use crate::repair::recovery;
use crate::vendor::core::{FileMeta, NzbFile, Par2Proof, Segment, Segments};
use crate::vendor::nntp::Body;
use crate::vendor::par2::{self, hash_16k};
use crate::vendor::queue::import::{
    self, Error as ImportError, Imported, NameSource, Progress, Rules,
};
use crate::vendor::queue::names::{self, Kind};
use crate::vendor::queue::nzb::{File, Nzb};

const FIRSTS: usize = 16;

pub enum Next {
    Plan(Plan),
    Raw,
    Fail(String),
}

pub async fn direct(fetcher: &Fetcher, nzb: &Nzb, name: &str, password: Option<&str>) -> Next {
    let rules = Rules {
        check_percent: 0,
        concurrency: ImportMode::Download.concurrency(),
        ..Rules::default()
    };
    let imported = import::import(
        &fetcher.client,
        nzb,
        name,
        "",
        &rules,
        password,
        Arc::new(Progress::default()),
        None,
    )
    .await;
    match imported {
        Ok(Imported::Files { outputs, .. }) if outputs.iter().any(|o| o.meta.encrypted()) => {
            Next::Raw
        }
        Ok(Imported::Files { outputs, .. }) => Next::Plan(Plan {
            raw: false,
            probed: false,
            outputs: outputs
                .into_iter()
                .map(|o| Planned {
                    path: o.path.join("/"),
                    member: o.origin.source == NameSource::ArchiveHeader,
                    crc32: o.crc32,
                    meta: o.meta,
                })
                .collect(),
        }),
        Ok(Imported::Nzb(_)) => {
            Next::Fail("The download holds another NZB instead of a release.".to_owned())
        }
        Err(ImportError::Password(reason)) => Next::Fail(password_failure(password, &reason)),
        Err(ImportError::Unsupported(reason)) => {
            tracing::info!(%reason, "downloading whole volumes to unpack them");
            Next::Raw
        }
        Err(ImportError::Missing(reason)) if has_par2(nzb) => {
            tracing::info!(%reason, "downloading whole volumes to repair them");
            Next::Raw
        }
        Err(error) => Next::Fail(error.to_string()),
    }
}

pub fn password_failure(password: Option<&str>, reason: &str) -> String {
    match password {
        None => format!(
            "The archive is password-protected and no password was given (NZB meta or job): {reason}"
        ),
        Some(_) => format!("Wrong archive password: {reason}"),
    }
}

pub fn has_par2(nzb: &Nzb) -> bool {
    recovery::par2_files(nzb).next().is_some()
}

pub async fn raw(fetcher: &Fetcher, nzb: &Nzb) -> Result<Plan, String> {
    let index = recovery::index(fetcher, nzb)
        .await
        .map_err(|e| format!("providers unavailable: {e}"))?;
    let described = index.map(|r| r.index());
    let slice_size = described.as_ref().map_or(0, |i| i.slice_size);
    let files: Vec<&par2::File> = described.iter().flat_map(|i| &i.files).collect();
    let data: Vec<&File> = nzb
        .files
        .iter()
        .filter(|f| names::classify(f.name()) != Kind::Par2)
        .collect();
    let requests: Vec<_> = data
        .iter()
        .map(|f| {
            let (fetcher, id) = (fetcher.clone(), f.segments[0].id.clone());
            async move { fetcher.body(&id).await }
        })
        .collect();
    let firsts: Vec<_> = stream::iter(requests).buffered(FIRSTS).collect().await;
    let mut taken = HashSet::new();
    let mut outputs = Vec::new();
    for (file, first) in data.into_iter().zip(firsts) {
        let first = match first.map_err(|e| format!("providers unavailable: {e}"))? {
            Outcome::Body(body) => body,
            Outcome::Missing | Outcome::Corrupt => {
                return Err(format!(
                    "{}: its first article is missing on every provider",
                    file.name()
                ));
            }
        };
        let segments = uniform(file, &first)?;
        let start = &first.data[..first.data.len().min(16 << 10)];
        let matched = files
            .iter()
            .find(|f| f.hash_16k == hash_16k(start) && f.length == first.file_size);
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

fn uniform(file: &File, first: &Body) -> Result<Segments, String> {
    let count = file.segments.len() as u64;
    let part = first.data.len() as u64;
    let last = first
        .file_size
        .checked_sub(part * (count - 1))
        .filter(|&last| first.offset == 0 && last > 0 && last <= part)
        .ok_or_else(|| {
            format!(
                "{}: its articles are not split evenly, which whole-volume downloads need",
                file.name()
            )
        })?;
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
