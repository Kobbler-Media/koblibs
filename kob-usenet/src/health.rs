use std::collections::HashSet;
use std::sync::Arc;

use futures_util::{StreamExt, stream};

use crate::Error;
use crate::store::Layout;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::{self, Client, Lane, Workload};
use crate::vendor::queue::import::Error as ImportError;

const CONCURRENCY: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthSample {
    pub checked: usize,
    pub missing: usize,
}

pub async fn sample(
    client: &Arc<Client>,
    layout: &Layout,
    percent: u8,
) -> Result<HealthSample, Error> {
    let mut seen = HashSet::new();
    let ids: Vec<&str> = layout
        .outputs
        .iter()
        .flat_map(|(_, meta)| ids(meta))
        .filter(|id| !id.is_empty() && seen.insert(*id))
        .collect();
    let wanted = (ids.len() * usize::from(percent.min(100)))
        .div_ceil(100)
        .clamp(ids.len().min(1), ids.len());
    let lane = Lane::default();
    let answers: Vec<Result<bool, nntp::Error>> =
        stream::iter((0..wanted).map(|i| ids[i * ids.len() / wanted]))
            .map(|id| async move {
                match client
                    .stat_in_lane(id, lane, Workload::Health, None, layout.posted)
                    .await
                {
                    Ok(()) => Ok(true),
                    Err(nntp::Error::NotFound) => Ok(false),
                    Err(error) => Err(error),
                }
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
    let mut sample = HealthSample::default();
    for answer in answers {
        let found = answer.map_err(|e| Error::Import(ImportError::Retry(e)))?;
        sample.checked += 1;
        sample.missing += usize::from(!found);
    }
    Ok(sample)
}

fn ids(meta: &FileMeta) -> Vec<&str> {
    match meta {
        FileMeta::Nzb(file) => file.segments.items.iter().map(|s| s.id.as_str()).collect(),
        FileMeta::Multipart(file) => file
            .parts
            .iter()
            .flat_map(|p| p.segments.items.iter().map(|s| s.id.as_str()))
            .collect(),
    }
}
