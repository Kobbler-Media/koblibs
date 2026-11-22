use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::jobs::{JobSpec, JobStatus};
use crate::release::hex;
use crate::store::{sweep_tmp, write_atomic};
use crate::vendor::core::{FileMeta, blob};
use crate::vendor::queue::names;

const PLAN_VERSION: u16 = 1;
const STAGING: &str = ".incomplete";

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSpec {
    pub seq: u64,
    pub id: String,
    pub name: String,
    pub category: String,
    pub password: Option<String>,
    pub dest_dir: PathBuf,
    pub folder: String,
}

impl StoredSpec {
    pub fn new(seq: u64, spec: &JobSpec) -> Self {
        let folder = names::sanitize(&spec.name)
            .filter(|f| !f.starts_with('.'))
            .unwrap_or_else(|| hex(&Sha256::digest(spec.id.as_bytes()))[..16].to_owned());
        Self {
            seq,
            id: spec.id.clone(),
            name: spec.name.clone(),
            category: spec.category.clone(),
            password: spec.password.clone(),
            dest_dir: spec.dest_dir.clone(),
            folder,
        }
    }

    pub fn staging(&self) -> PathBuf {
        self.dest_dir.join(STAGING).join(&self.folder)
    }
}

#[derive(Clone, Debug)]
pub struct Planned {
    pub path: String,
    pub meta: FileMeta,
    pub crc32: Option<u32>,
    pub member: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub raw: bool,
    pub probed: bool,
    pub outputs: Vec<Planned>,
}

#[derive(Serialize, Deserialize)]
struct StoredPlan {
    version: u16,
    raw: bool,
    probed: bool,
    outputs: Vec<(String, Vec<u8>, Option<u32>, bool)>,
}

pub struct JobStore {
    root: PathBuf,
}

impl JobStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let root = data_dir.join("jobs");
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(hex(&Sha256::digest(id.as_bytes())))
    }

    pub fn create(&self, spec: &StoredSpec, nzb: &[u8], status: &JobStatus) -> Result<(), Error> {
        let dir = self.dir(&spec.id);
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("nzb"), nzb)?;
        write_atomic(&dir.join("spec"), &encode(spec)?)?;
        self.put_status(status)
    }

    pub fn load(&self) -> Result<Vec<(StoredSpec, JobStatus)>, Error> {
        let mut jobs = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let dir = entry?.path();
            if !dir.is_dir() {
                continue;
            }
            sweep_tmp(&dir)?;
            let read = |name: &str| fs::read(dir.join(name));
            let (Ok(spec), Ok(status)) = (read("spec"), read("status")) else {
                tracing::warn!(dir = %dir.display(), "skipping an incomplete job record");
                continue;
            };
            jobs.push((decode(&spec)?, decode(&status)?));
        }
        Ok(jobs)
    }

    pub fn spec(&self, id: &str) -> Result<StoredSpec, Error> {
        decode(&fs::read(self.dir(id).join("spec"))?)
    }

    pub fn nzb(&self, id: &str) -> io::Result<Vec<u8>> {
        fs::read(self.dir(id).join("nzb"))
    }

    pub fn put_nzb(&self, id: &str, nzb: &[u8]) -> io::Result<()> {
        write_atomic(&self.dir(id).join("nzb"), nzb)
    }

    pub fn put_status(&self, status: &JobStatus) -> Result<(), Error> {
        Ok(write_atomic(
            &self.dir(&status.id).join("status"),
            &encode(status)?,
        )?)
    }

    pub fn plan(&self, id: &str) -> Result<Option<Plan>, Error> {
        let bytes = match fs::read(self.dir(id).join("plan")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let stored: StoredPlan = decode(&bytes)?;
        if stored.version != PLAN_VERSION {
            return Err(Error::Stored(format!(
                "job plan version {}",
                stored.version
            )));
        }
        let outputs = stored
            .outputs
            .into_iter()
            .map(|(path, meta, crc32, member)| {
                Ok(Planned {
                    path,
                    meta: blob::decode_meta(&meta)?,
                    crc32,
                    member,
                })
            })
            .collect::<Result<_, blob::Error>>()
            .map_err(|e| Error::Stored(e.to_string()))?;
        Ok(Some(Plan {
            raw: stored.raw,
            probed: stored.probed,
            outputs,
        }))
    }

    pub fn put_plan(&self, id: &str, plan: &Plan) -> Result<(), Error> {
        let outputs = plan
            .outputs
            .iter()
            .map(|o| {
                Ok((
                    o.path.clone(),
                    blob::encode_meta(&o.meta)?,
                    o.crc32,
                    o.member,
                ))
            })
            .collect::<Result<_, blob::Error>>()
            .map_err(|e| Error::Stored(e.to_string()))?;
        let stored = StoredPlan {
            version: PLAN_VERSION,
            raw: plan.raw,
            probed: plan.probed,
            outputs,
        };
        Ok(write_atomic(&self.dir(id).join("plan"), &encode(&stored)?)?)
    }

    pub fn map(&self, id: &str, output: usize) -> PathBuf {
        self.dir(id).join(format!("map-{output}"))
    }

    pub fn clear_maps(&self, id: &str) -> io::Result<()> {
        for entry in fs::read_dir(self.dir(id))? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("map-"))
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn remove(&self, id: &str) -> io::Result<()> {
        fs::remove_dir_all(self.dir(id))
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    postcard::to_stdvec(value).map_err(|e| Error::Stored(e.to_string()))
}

fn decode<T: for<'a> Deserialize<'a>>(bytes: &[u8]) -> Result<T, Error> {
    postcard::from_bytes(bytes).map_err(|e| Error::Stored(e.to_string()))
}
