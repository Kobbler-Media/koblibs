use std::path::PathBuf;

use crate::repair::control::Ticket;
use crate::repair::sets::{position, usable};
use crate::repair::{OnDisk, RepairError, damaged_slices};
use crate::vendor::par2::{self, Id, Reader};

pub struct Contested {
    name: String,
    path: PathBuf,
    claims: Vec<(par2::File, u64)>,
}

pub fn contested(reader: &Reader, files: &[OnDisk]) -> Vec<Contested> {
    let plans: Vec<_> = reader
        .sets()
        .into_iter()
        .filter_map(|set| usable(reader, set))
        .collect();
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.damaged)
        .filter_map(|(at, file)| {
            let mut hashes: Vec<Id> = Vec::new();
            let mut claims = Vec::new();
            for plan in &plans {
                for source in plan.sources() {
                    if position(&source.file, files) == Some(at)
                        && !hashes.contains(&source.file.hash)
                    {
                        hashes.push(source.file.hash);
                        claims.push((source.file.clone(), plan.slice_size() as u64));
                    }
                }
            }
            (claims.len() > 1).then(|| Contested {
                name: file.name.clone(),
                path: file.path.clone(),
                claims,
            })
        })
        .collect()
}

// Rival sets can each be self-consistent and only one is the release, and nothing on disk says
// which. A claim the damaged bytes already satisfy describes the hole itself, so it is no rival.
pub fn undecidable(
    contested: &[Contested],
    ticket: &Ticket,
) -> Result<Option<String>, RepairError> {
    for file in contested {
        let mut standing = 0;
        for (claim, slice_size) in &file.claims {
            let bad = damaged_slices(
                &file.path,
                claim.length,
                *slice_size,
                &claim.slices,
                &claim.hash,
                &|| ticket.cancelled(),
            )
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::Interrupted => RepairError::Cancelled,
                _ => RepairError::Io(e),
            })?;
            standing += usize::from(!bad.is_empty());
        }
        if standing > 1 {
            return Ok(Some(file.name.clone()));
        }
    }
    Ok(None)
}
