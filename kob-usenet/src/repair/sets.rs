use std::collections::HashSet;
use std::path::Path;

use crate::repair::spool::MAX_SLICE;
use crate::repair::{OnDisk, RepairError, Source};
use crate::vendor::par2::{self, Id, Reader};

pub fn usable(reader: &Reader, set: Id) -> Option<par2::RepairSet<'_>> {
    reader
        .verification_set(set)
        .ok()
        .filter(|plan| plan.slice_size() as u64 <= MAX_SLICE)
}

pub fn described(reader: &Reader) -> Vec<(par2::File, u64)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for set in reader.sets() {
        if let Some(plan) = usable(reader, set) {
            for source in plan.sources() {
                if seen.insert(source.file.id) {
                    out.push((source.file.clone(), plan.slice_size() as u64));
                }
            }
        }
    }
    let index = reader.index();
    if index.slice_size <= MAX_SLICE {
        for file in index.files {
            if seen.insert(file.id) {
                out.push((file, index.slice_size));
            }
        }
    }
    out
}

pub fn position(file: &par2::File, files: &[OnDisk]) -> Option<usize> {
    files
        .iter()
        .position(|d| d.id == Some(file.id))
        .or_else(|| files.iter().position(|d| d.name == file.name))
}

pub fn covered(reader: &Reader, files: &[OnDisk]) -> bool {
    let plans: Vec<_> = reader
        .sets()
        .into_iter()
        .filter_map(|set| usable(reader, set))
        .collect();
    files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.damaged)
        .all(|(at, _)| {
            plans.iter().any(|plan| {
                plan.sources()
                    .iter()
                    .any(|s| position(&s.file, files) == Some(at))
            })
        })
}

pub fn covering(reader: &Reader, files: &[OnDisk]) -> Vec<Id> {
    let mut sets: Vec<(bool, Id)> = reader
        .sets()
        .into_iter()
        .filter_map(|set| {
            let plan = usable(reader, set)?;
            let holds: Vec<usize> = plan
                .sources()
                .iter()
                .filter_map(|s| position(&s.file, files).filter(|&at| files[at].damaged))
                .collect();
            if holds.is_empty() {
                return None;
            }
            let proven = plan.sources().iter().any(|s| {
                files
                    .iter()
                    .any(|d| d.damaged && d.hash == Some(s.file.hash))
            });
            Some((!proven, set))
        })
        .collect();
    sets.sort();
    sets.into_iter().map(|(_, set)| set).collect()
}

pub fn sources(
    plan: &par2::RepairSet<'_>,
    files: &[OnDisk],
    staging: &Path,
    absent_ok: bool,
) -> Result<Vec<Source>, RepairError> {
    plan.sources()
        .iter()
        .map(|source| {
            let found = position(&source.file, files).map(|at| &files[at]);
            let path = match found {
                Some(d) => d.path.clone(),
                None if absent_ok => {
                    let name = crate::vendor::queue::names::sanitize(&source.file.name)
                        .ok_or_else(|| RepairError::Uncovered(source.file.name.clone()))?;
                    staging.join(name)
                }
                None => return Err(RepairError::Uncovered(source.file.name.clone())),
            };
            Ok(Source {
                file: source.file.clone(),
                slices: source.slices.clone(),
                present: found.is_some(),
                trusted: found.is_some_and(|d| d.id.is_some() && !d.damaged),
                path,
            })
        })
        .collect()
}
