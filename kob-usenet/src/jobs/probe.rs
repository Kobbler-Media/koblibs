use std::collections::HashSet;

use futures_util::{StreamExt, stream};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::store::Plan;
use crate::repair::recovery::Par2s;
use crate::vendor::core::{FileMeta, Segments};
use crate::vendor::queue::nzb::Nzb;

const ALL_BELOW: usize = 200;
const SAMPLE_PERCENT: usize = 2;
const SAMPLE_MAX: usize = 1000;

pub struct Missed {
    pub members: bool,
}

pub async fn hopeless(
    fetcher: &Fetcher,
    nzb: &Nzb,
    par2: &Par2s,
    plan: &Plan,
) -> Result<Missed, Failure> {
    let mut seen = HashSet::new();
    let segments: Vec<(&str, u32, bool)> = plan
        .outputs
        .iter()
        .flat_map(|o| {
            segments(&o.meta)
                .into_iter()
                .map(move |(id, size)| (id, size, o.member))
        })
        .filter(|(id, _, _)| !id.is_empty() && seen.insert(*id))
        .collect();
    let clean = Missed { members: false };
    if segments.is_empty() {
        return Ok(clean);
    }
    let total = segments.len();
    let wanted = if total <= ALL_BELOW {
        total
    } else {
        (total * SAMPLE_PERCENT / 100).clamp(ALL_BELOW, SAMPLE_MAX)
    };
    let sample: Vec<usize> = (0..wanted).map(|i| i * total / wanted).collect();
    let stats: Vec<_> = sample
        .iter()
        .map(|&i| {
            let (fetcher, id) = (fetcher.clone(), segments[i].0.to_owned());
            async move { fetcher.stat(&id).await }
        })
        .collect();
    let answers: Vec<_> = stream::iter(stats)
        .buffered(fetcher.planning_breadth())
        .collect()
        .await;
    if let Some(Err(error)) = answers.iter().find(|a| a.is_err()) {
        return Err(Failure::providers(error));
    }
    let missing: u64 = sample
        .iter()
        .zip(&answers)
        .filter(|(_, a)| matches!(a, Ok(false)))
        .map(|(&i, _)| u64::from(segments[i].1))
        .sum();
    let missed = answers.iter().filter(|a| matches!(a, Ok(false))).count();
    if missed == 0 {
        return Ok(clean);
    }
    let members = sample
        .iter()
        .zip(&answers)
        .any(|(&i, a)| matches!(a, Ok(false)) && segments[i].2);
    let slice_size = plan.outputs.iter().find_map(|o| slice_size(&o.meta));
    let recovery_blocks = par2.recovery_blocks(nzb, slice_size.map_or(1 << 20, |s| s as usize));
    let estimated = missing * total as u64 / wanted as u64;
    let need = slice_size.map(|slice| usize::try_from(estimated / slice).unwrap_or(usize::MAX));
    let hopeless = match need {
        _ if recovery_blocks == 0 => true,
        Some(need) => need > recovery_blocks,
        None => false,
    };
    if !hopeless {
        return Ok(Missed { members });
    }
    Err(Failure::Content(format!(
        "The job is hopeless: {missed} of {wanted} sampled articles are missing (at least {} PAR2 blocks) \
             but only {recovery_blocks} recovery blocks are posted.",
        need.map_or_else(|| "unknown".to_owned(), |n| n.to_string())
    )))
}

fn segments(meta: &FileMeta) -> Vec<(&str, u32)> {
    let spans: Vec<&Segments> = match meta {
        FileMeta::Nzb(f) => vec![&f.segments],
        FileMeta::Multipart(m) => m.parts.iter().map(|p| &p.segments).collect(),
    };
    spans
        .into_iter()
        .flat_map(|s| s.items.iter().map(|s| (s.id.as_str(), s.size)))
        .collect()
}

fn slice_size(meta: &FileMeta) -> Option<u64> {
    let proof = match meta {
        FileMeta::Nzb(f) => f.proof.as_ref(),
        FileMeta::Multipart(m) => m.parts.iter().find_map(|p| p.proof.as_ref()),
    };
    proof.map(|p| u64::from(p.slice_size)).filter(|&s| s > 0)
}
