use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::release::{ReleaseKey, hex};
use crate::vendor::core::FileMeta;
use crate::vendor::core::blob;

const LAYOUT_VERSION: u16 = 1;

pub struct Store {
    root: PathBuf,
    nzb_cap: u64,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub posted: Option<i64>,
    pub outputs: Vec<(String, FileMeta)>,
}

#[derive(Serialize, Deserialize)]
struct StoredLayout {
    version: u16,
    posted: Option<i64>,
    outputs: Vec<(String, Vec<u8>)>,
}

impl Store {
    pub fn open(root: &Path, nzb_cap: u64) -> io::Result<Self> {
        for dir in ["nzb", "alias", "release"] {
            fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            nzb_cap,
        })
    }

    fn alias_path(&self, cache_key: &str) -> PathBuf {
        self.root
            .join("alias")
            .join(hex(&Sha256::digest(cache_key.as_bytes())))
    }

    fn nzb_path(&self, key: &ReleaseKey) -> PathBuf {
        self.root.join("nzb").join(key.as_str())
    }

    fn layout_path(&self, key: &ReleaseKey) -> PathBuf {
        self.root.join("release").join(key.as_str())
    }

    fn usage_path(&self) -> PathBuf {
        self.root.join("usage")
    }

    pub fn alias(&self, cache_key: &str) -> Option<ReleaseKey> {
        fs::read_to_string(self.alias_path(cache_key))
            .ok()?
            .parse()
            .ok()
    }

    pub fn set_alias(&self, cache_key: &str, key: &ReleaseKey) -> io::Result<()> {
        write_atomic(&self.alias_path(cache_key), key.as_str().as_bytes())
    }

    pub fn nzb(&self, key: &ReleaseKey) -> Option<Vec<u8>> {
        let path = self.nzb_path(key);
        let xml = blob::decode_nzb(&fs::read(&path).ok()?).ok()?;
        if let Ok(file) = fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(xml)
    }

    pub fn put_nzb(&self, key: &ReleaseKey, xml: &[u8]) -> Result<(), Error> {
        let encoded = blob::encode_nzb(xml).map_err(|e| Error::Stored(e.to_string()))?;
        let path = self.nzb_path(key);
        write_atomic(&path, &encoded)?;
        self.evict_nzbs(&path)?;
        Ok(())
    }

    fn evict_nzbs(&self, keep: &Path) -> io::Result<()> {
        let mut cached: Vec<(SystemTime, u64, PathBuf)> = fs::read_dir(self.root.join("nzb"))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((meta.modified().ok()?, meta.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = cached.iter().map(|(_, len, _)| len).sum();
        cached.sort();
        for (_, len, path) in cached {
            if total <= self.nzb_cap {
                break;
            }
            if path != keep && fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }

    pub fn layout(&self, key: &ReleaseKey) -> Result<Option<Layout>, Error> {
        let bytes = match fs::read(self.layout_path(key)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let stored: StoredLayout =
            postcard::from_bytes(&bytes).map_err(|e| Error::Stored(e.to_string()))?;
        if stored.version != LAYOUT_VERSION {
            return Err(Error::Stored(format!("layout version {}", stored.version)));
        }
        let outputs = stored
            .outputs
            .into_iter()
            .map(|(path, meta)| Ok((path, blob::decode_meta(&meta)?)))
            .collect::<Result<_, blob::Error>>()
            .map_err(|e| Error::Stored(e.to_string()))?;
        Ok(Some(Layout {
            posted: stored.posted,
            outputs,
        }))
    }

    pub fn put_layout(&self, key: &ReleaseKey, layout: &Layout) -> Result<(), Error> {
        let outputs = layout
            .outputs
            .iter()
            .map(|(path, meta)| Ok((path.clone(), blob::encode_meta(meta)?)))
            .collect::<Result<_, blob::Error>>()
            .map_err(|e| Error::Stored(e.to_string()))?;
        let stored = StoredLayout {
            version: LAYOUT_VERSION,
            posted: layout.posted,
            outputs,
        };
        let bytes = postcard::to_stdvec(&stored).map_err(|e| Error::Stored(e.to_string()))?;
        Ok(write_atomic(&self.layout_path(key), &bytes)?)
    }

    pub fn usage(&self) -> HashMap<String, u64> {
        fs::read(self.usage_path())
            .ok()
            .and_then(|bytes| postcard::from_bytes(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn put_usage(&self, usage: &HashMap<String, u64>) -> Result<(), Error> {
        let bytes = postcard::to_stdvec(usage).map_err(|e| Error::Stored(e.to_string()))?;
        Ok(write_atomic(&self.usage_path(), &bytes)?)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
