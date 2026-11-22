mod files;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::release::{ReleaseKey, hex};
use crate::vendor::core::FileMeta;
use crate::vendor::core::blob;
pub(crate) use files::{sweep_tmp, write_atomic};

const LAYOUT_VERSION: u16 = 1;
const ALIASES: usize = 100_000;
const DIRS: [&str; 3] = ["nzb", "alias", "release"];

pub struct Store {
    root: PathBuf,
    nzb_cap: u64,
    release_cap: u64,
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
    pub fn open(root: &Path, nzb_cap: u64, release_cap: u64) -> io::Result<Self> {
        sweep_tmp(root).or_else(|e| match e.kind() {
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(e),
        })?;
        for dir in DIRS {
            fs::create_dir_all(root.join(dir))?;
            sweep_tmp(&root.join(dir))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            nzb_cap,
            release_cap,
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

    fn usage_backup(&self) -> PathBuf {
        self.root.join("usage.bak")
    }

    pub fn alias(&self, cache_key: &str) -> Option<ReleaseKey> {
        let path = self.alias_path(cache_key);
        let key = fs::read_to_string(&path).ok()?.parse().ok()?;
        files::touch(&path);
        Some(key)
    }

    pub fn set_alias(&self, cache_key: &str, key: &ReleaseKey) -> io::Result<()> {
        write_atomic(&self.alias_path(cache_key), key.as_str().as_bytes())
    }

    pub fn nzb(&self, key: &ReleaseKey) -> Option<Vec<u8>> {
        let path = self.nzb_path(key);
        let xml = blob::decode_nzb(&fs::read(&path).ok()?).ok()?;
        files::touch(&path);
        Some(xml)
    }

    pub fn put_nzb(&self, key: &ReleaseKey, xml: &[u8]) -> Result<(), Error> {
        let encoded = blob::encode_nzb(xml).map_err(|e| Error::Stored(e.to_string()))?;
        let path = self.nzb_path(key);
        write_atomic(&path, &encoded)?;
        files::evict(
            &self.root.join("nzb"),
            self.nzb_cap,
            usize::MAX,
            Some(&path),
        )?;
        Ok(())
    }

    pub fn layout(&self, key: &ReleaseKey) -> Result<Option<Layout>, Error> {
        let path = self.layout_path(key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match decode_layout(&bytes) {
            Ok(layout) => {
                files::touch(&path);
                Ok(Some(layout))
            }
            Err(reason) => {
                tracing::warn!(release = %key, %reason, "dropping an unreadable stored layout; the release imports again");
                fs::remove_file(&path)?;
                Ok(None)
            }
        }
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

    pub fn sweep(&self) -> io::Result<()> {
        files::evict(
            &self.root.join("release"),
            self.release_cap,
            usize::MAX,
            None,
        )?;
        files::evict(&self.root.join("alias"), u64::MAX, ALIASES, None)
    }

    pub fn usage(&self) -> Result<HashMap<String, u64>, Error> {
        let read = |path: &Path| match fs::read(path) {
            Ok(bytes) => Some(postcard::from_bytes(&bytes).map_err(|e| e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => Some(Err(e.to_string())),
        };
        match (read(&self.usage_path()), read(&self.usage_backup())) {
            (None, _) => Ok(HashMap::new()),
            (Some(Ok(usage)), _) => Ok(usage),
            (Some(Err(reason)), Some(Ok(usage))) => {
                tracing::error!(%reason, "provider usage file is unreadable; using its backup");
                Ok(usage)
            }
            (Some(Err(reason)), _) => Err(Error::Stored(format!(
                "provider usage file {} is unreadable ({reason}) and has no readable backup; \
                 fix or remove it to reset block-account quotas",
                self.usage_path().display()
            ))),
        }
    }

    pub fn put_usage(&self, usage: &HashMap<String, u64>) -> Result<(), Error> {
        let bytes = postcard::to_stdvec(usage).map_err(|e| Error::Stored(e.to_string()))?;
        if let Ok(previous) = fs::read(self.usage_path())
            && postcard::from_bytes::<HashMap<String, u64>>(&previous).is_ok()
        {
            write_atomic(&self.usage_backup(), &previous)?;
        }
        Ok(write_atomic(&self.usage_path(), &bytes)?)
    }
}

fn decode_layout(bytes: &[u8]) -> Result<Layout, String> {
    let stored: StoredLayout = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    if stored.version != LAYOUT_VERSION {
        return Err(format!("layout version {}", stored.version));
    }
    let outputs = stored
        .outputs
        .into_iter()
        .map(|(path, meta)| Ok((path, blob::decode_meta(&meta)?)))
        .collect::<Result<_, blob::Error>>()
        .map_err(|e| e.to_string())?;
    Ok(Layout {
        posted: stored.posted,
        outputs,
    })
}
