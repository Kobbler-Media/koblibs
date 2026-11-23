use std::collections::HashSet;
use std::sync::Arc;

use futures_util::{StreamExt, stream};

use crate::Error;
use crate::store::Layout;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::{self, Client, Lane, Workload};
use crate::vendor::queue::import::Error as ImportError;

const CONCURRENCY: usize = 16;
const QUICK_CONCURRENCY: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthSample {
    pub checked: usize,
    pub missing: usize,
}

pub async fn sample(
    client: &Arc<Client>,
    layout: &Layout,
    percent: u8,
    output: Option<usize>,
) -> Result<HealthSample, Error> {
    let ids = article_ids(layout, output);
    let wanted = (ids.len() * usize::from(percent.min(100)))
        .div_ceil(100)
        .clamp(ids.len().min(1), ids.len());
    let picked = (0..wanted).map(|i| ids[i * ids.len() / wanted].clone());
    let posted = layout.posted;
    tally(picked, CONCURRENCY, |id| {
        let client = Arc::clone(client);
        async move {
            client
                .stat_in_lane(&id, Lane::default(), Workload::Health, None, posted)
                .await
        }
    })
    .await
}

pub async fn quick(
    client: &Arc<Client>,
    layout: &Layout,
    articles: usize,
    output: Option<usize>,
) -> Result<HealthSample, Error> {
    let ids = article_ids(layout, output);
    let posted = layout.posted;
    let picked = spread(ids.len(), articles).map(|i| ids[i].clone());
    tally(picked, QUICK_CONCURRENCY, |id| {
        let client = Arc::clone(client);
        async move { client.stat_quick(&id, posted).await }
    })
    .await
}

fn spread(len: usize, wanted: usize) -> impl Iterator<Item = usize> {
    let wanted = wanted.clamp(len.min(2), len);
    (0..wanted).map(move |i| match wanted {
        1 => 0,
        _ => i * (len - 1) / (wanted - 1),
    })
}

fn article_ids(layout: &Layout, output: Option<usize>) -> Vec<String> {
    let mut seen = HashSet::new();
    layout
        .outputs
        .iter()
        .enumerate()
        .filter(|(i, _)| output.is_none_or(|o| o == *i))
        .flat_map(|(_, (_, meta))| ids(meta))
        .filter(|id| !id.is_empty() && seen.insert(*id))
        .map(str::to_owned)
        .collect()
}

async fn tally<F, Fut>(
    picked: impl Iterator<Item = String>,
    concurrency: usize,
    stat: F,
) -> Result<HealthSample, Error>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), nntp::Error>>,
{
    let answers: Vec<_> = stream::iter(picked)
        .map(stat)
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let mut sample = HealthSample::default();
    for answer in answers {
        let found = match answer {
            Ok(()) => true,
            Err(nntp::Error::NotFound) => false,
            Err(error) => return Err(Error::Import(ImportError::Retry(error))),
        };
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
