pub mod reader;

pub use reader::Reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("stored blob is unreadable")]
    Blob(#[from] crate::vendor::core::blob::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(&'static str),
}
