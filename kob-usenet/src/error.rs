use crate::release::ReleaseKey;
use crate::vendor;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid NZB: {0}")]
    Nzb(#[from] vendor::queue::nzb::Error),
    #[error("no NZB given and none cached for this key")]
    NotCached,
    #[error("the NZB holds another NZB instead of a release")]
    NestedNzb,
    #[error("no configured server retains articles this old")]
    Retention,
    #[error("unknown release {0}")]
    UnknownRelease(ReleaseKey),
    #[error("release {key} has no output {index}")]
    UnknownOutput { key: ReleaseKey, index: usize },
    #[error("import failed: {0}")]
    Import(#[from] vendor::queue::import::Error),
    #[error("server {server}: {source}")]
    Server {
        server: String,
        source: vendor::nntp::Error,
    },
    #[error("stored release is unreadable: {0}")]
    Stored(String),
    #[error("reader: {0}")]
    Reader(#[from] vendor::vfs::Error),
    #[error("data directory: {0}")]
    Io(#[from] std::io::Error),
}
