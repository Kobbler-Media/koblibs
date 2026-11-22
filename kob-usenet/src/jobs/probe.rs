use std::collections::HashSet;

use futures_util::{StreamExt, stream};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::store::Plan;
use crate::repair::recovery;
use crate::vendor::core::{FileMeta, Segments};
use crate::vendor::queue::nzb::Nzb;

const ALL_BELOW: usize = 200;
const SAMPLE_PERCENT: usize = 2;
const SAMPLE_MAX: usize = 1000;
const CONCURRENCY: usize = 16;

pub async fn hopeless(fetcher: &Fetcher, nzb: &Nzb, plan: &Plan) -> Result<(), Failure> {
    let mut seen = HashSet::new();
    let segments: Vec<(&str, u32)> = plan
        .outputs
        .iter()
        .flat_map(|o| segments(&o.meta))
        .filter(|(id, _)| !id.is_empty() && seen.insert(*id))
        .collect();
    if segments.is_empty() {
        return Ok(());
    }
    let total = segments.len();
    let wanted = if total <= ALL_BELOW {
        total
    } else {
        (total * SAMPLE_PERCENT / 100).clamp(ALL_BELOW, SAMPLE_MAX)
    };
    let sample: Vec<&str> = (0..wanted)
        .map(|i| segments[i * total / wanted].0)
        .collect();
    let stats: Vec<_> = sample
        .iter()
        .map(|&id| {
            let (fetcher, id) = (fetcher.clone(), id.to_owned());
            async move { fetcher.stat(&id).await }
        })
        .collect();
    let answers: Vec<_> = stream::iter(stats)
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    if let Some(Err(error)) = answers.iter().find(|a| a.is_err()) {
        return Err(Failure::providers(error));
    }
    let missing = answers.iter().filter(|a| matches!(a, Ok(false))).count();
    if missing == 0 {
        return Ok(());
    }
    let slice_size = plan.outputs.iter().find_map(|o| slice_size(&o.meta));
    let recovery_blocks =
        recovery::recovery_blocks(nzb, slice_size.map_or(1 << 20, |s| s as usize));
    let estimated = (missing * total).div_ceil(wanted);
    let mean = segments
        .iter()
        .map(|&(_, size)| u64::from(size))
        .sum::<u64>()
        / total as u64;
    let need = slice_size
        .map(|slice| estimated * usize::try_from(mean.div_ceil(slice)).unwrap_or(usize::MAX));
    let verdict = match need {
        _ if recovery_blocks == 0 => true,
        Some(need) => need > recovery_blocks,
        None => false,
    };
    if !verdict {
        return Ok(());
    }
    Err(Failure::Content(format!(
        "The job is hopeless: {missing} of {wanted} sampled articles are missing (about {} PAR2 blocks) \
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
