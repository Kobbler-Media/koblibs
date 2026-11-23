use std::sync::Arc;

use crate::Purpose;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::{Client, Workload};
use crate::vendor::stream::{Budget, Cache};
use crate::vendor::vfs::{self, Reader};

pub struct Flights {
    cache: Arc<Cache>,
    budget: Budget,
    read_ahead: u64,
    cache_read_ahead: u64,
}

impl Flights {
    pub fn new(read_ahead: u64, cache_read_ahead: u64, cache: u64, budget: usize) -> Self {
        Self {
            cache: Arc::new(Cache::new(true, cache)),
            budget: Budget::new(budget),
            read_ahead,
            cache_read_ahead: cache_read_ahead.min(budget as u64),
        }
    }

    pub fn reader(
        &self,
        client: &Arc<Client>,
        meta: &FileMeta,
        purpose: Purpose,
    ) -> Result<Reader, vfs::Error> {
        let read_ahead = match purpose {
            Purpose::Cache => self.cache_read_ahead,
            Purpose::Play | Purpose::Warm => self.read_ahead,
        };
        let reader = self.open(client, meta, read_ahead)?;
        Ok(match purpose {
            Purpose::Warm => reader.with_warming(),
            purpose => reader.with_workload(purpose.workload()).strict(true),
        })
    }

    pub fn import_reader(
        &self,
        client: &Arc<Client>,
        meta: &FileMeta,
    ) -> Result<Reader, vfs::Error> {
        Ok(self
            .open(client, meta, self.read_ahead)?
            .with_workload(Workload::Import)
            .strict(true))
    }

    fn open(
        &self,
        client: &Arc<Client>,
        meta: &FileMeta,
        read_ahead: u64,
    ) -> Result<Reader, vfs::Error> {
        Reader::new(
            meta,
            client,
            &self.budget,
            Some(&self.cache),
            read_ahead,
            None,
            None,
        )
    }
}
