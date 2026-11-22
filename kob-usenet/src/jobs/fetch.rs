use std::sync::Arc;
use std::time::Duration;

use crate::vendor::nntp::{Body, BodyExpectation, Client, Error, Lane, Workload};

pub const MESSAGE_ID: &str = "message-id";
const ATTEMPTS: u32 = 4;
const BACKOFF: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct Fetcher {
    pub client: Arc<Client>,
    pub lane: Lane,
    pub posted: Option<i64>,
}

pub enum Outcome {
    Body(Body),
    Missing,
    Corrupt,
}

pub fn classify(result: Result<Body, Error>) -> Result<Outcome, Error> {
    match result {
        Ok(body) => Ok(Outcome::Body(body)),
        Err(Error::NotFound) => Ok(Outcome::Missing),
        Err(
            Error::CrcMismatch { .. }
            | Error::Yenc(_)
            | Error::TooLarge
            | Error::InvalidArgument(MESSAGE_ID),
        ) => Ok(Outcome::Corrupt),
        Err(error) => Err(error),
    }
}

impl Fetcher {
    pub fn new(client: &Arc<Client>, posted: Option<i64>) -> Self {
        Self {
            client: Arc::clone(client),
            lane: Lane::default(),
            posted,
        }
    }

    pub fn expect(&self, expected: BodyExpectation) -> BodyExpectation {
        BodyExpectation {
            posted: self.posted,
            ..expected
        }
    }

    pub async fn once(
        &self,
        id: &str,
        lane: Lane,
        expected: BodyExpectation,
    ) -> Result<Outcome, Error> {
        classify(
            self.client
                .body_in_lane(id, lane, Workload::Queue, None, self.expect(expected))
                .await,
        )
    }

    pub async fn body(&self, id: &str) -> Result<Outcome, Error> {
        retry(|| self.once(id, self.lane, BodyExpectation::NONE)).await
    }

    pub async fn first(&self, id: &str, expected: BodyExpectation) -> Result<Outcome, Error> {
        match retry(|| self.once(id, self.lane, expected)).await? {
            // An NZB listing more or fewer segments than the post's total has no copy that
            // matches, as in the import; such a part 1 is taken as listed.
            Outcome::Missing => self.body(id).await,
            outcome => Ok(outcome),
        }
    }

    pub async fn stat(&self, id: &str) -> Result<bool, Error> {
        retry(|| async {
            match self
                .client
                .stat_in_lane(id, self.lane, Workload::Queue, None, self.posted)
                .await
            {
                Ok(()) => Ok(true),
                Err(Error::NotFound | Error::InvalidArgument(MESSAGE_ID)) => Ok(false),
                Err(error) => Err(error),
            }
        })
        .await
    }
}

async fn retry<T, F: Future<Output = Result<T, Error>>>(
    mut attempt: impl FnMut() -> F,
) -> Result<T, Error> {
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(error) if tries < ATTEMPTS => {
                tracing::debug!(%error, tries, "retrying an article request");
                tokio::time::sleep(BACKOFF * tries).await;
                tries += 1;
            }
            result => return result,
        }
    }
}
