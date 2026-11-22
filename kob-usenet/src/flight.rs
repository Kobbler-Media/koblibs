use std::sync::Arc;

use crate::Purpose;
use crate::vendor::core::FileMeta;
use crate::vendor::nntp::Client;
use crate::vendor::stream::{Budget, Cache};
use crate::vendor::vfs::{self, Reader};

pub struct Flights {
    cache: Arc<Cache>,
    budget: Budget,
    read_ahead: u64,
}

impl Flights {
    pub fn new(read_ahead: u64, cache: u64, budget: usize) -> Self {
        Self {
            cache: Arc::new(Cache::new(true, cache)),
            budget: Budget::new(budget),
            read_ahead,
        }
    }

    pub fn reader(
        &self,
        client: &Arc<Client>,
        meta: &FileMeta,
        purpose: Purpose,
    ) -> Result<Reader, vfs::Error> {
        let reader = Reader::new(
            meta,
            client,
            &self.budget,
            Some(&self.cache),
            self.read_ahead,
            None,
            None,
        )?;
        Ok(match purpose {
            Purpose::Warm => reader.with_warming(),
            purpose => reader.with_workload(purpose.workload()).strict(true),
        })
    }
}
