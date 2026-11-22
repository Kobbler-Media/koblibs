use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::Error;
use crate::vendor::queue::nzb::{self, Nzb};

const PREFIX: &str = "nzb-";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReleaseKey(String);

impl ReleaseKey {
    pub fn from_nzb(raw: &[u8]) -> Result<Self, Error> {
        Ok(Self::of(&nzb::parse(&nzb::decompress(raw)?)?))
    }

    #[must_use]
    pub fn of(nzb: &Nzb) -> Self {
        let mut firsts: Vec<&str> = nzb
            .files
            .iter()
            .filter_map(|f| f.segments.first())
            .map(|s| s.id.as_str())
            .collect();
        firsts.sort_unstable();
        let mut hash = Sha256::new();
        for id in firsts {
            hash.update((id.len() as u64).to_le_bytes());
            hash.update(id);
        }
        Self(format!("{PREFIX}{}", hex(&hash.finalize())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl fmt::Display for ReleaseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("not a release key: {0}")]
pub struct InvalidKey(String);

impl FromStr for ReleaseKey {
    type Err = InvalidKey;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.strip_prefix(PREFIX)
            .filter(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .map(|_| Self(s.to_owned()))
            .ok_or_else(|| InvalidKey(s.to_owned()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub key: ReleaseKey,
    pub outputs: Vec<OutputInfo>,
}
