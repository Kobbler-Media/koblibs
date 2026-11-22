use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::day;
use crate::space::{FreeSpace, Statvfs};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IpPreference {
    #[default]
    Any,
    V4,
    V6,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub verify_cert: bool,
    pub ip_preference: IpPreference,
    pub connections: usize,
    pub priority: u32,
    pub backbone: Option<String>,
    pub retention_days: Option<u32>,
    pub expires_on: Option<String>,
    pub quota_bytes: Option<u64>,
    pub username: String,
    pub password: String,
}

impl ServerConfig {
    #[must_use]
    pub fn expired(&self, today: i64) -> bool {
        self.expires_on
            .as_deref()
            .and_then(day::parse)
            .is_some_and(|last| today > last)
    }

    #[must_use]
    pub fn retains(&self, posted_day: i64, today: i64) -> bool {
        self.retention_days
            .is_none_or(|days| today - posted_day <= i64::from(days))
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("verify_cert", &self.verify_cert)
            .field("ip_preference", &self.ip_preference)
            .field("connections", &self.connections)
            .field("priority", &self.priority)
            .field("backbone", &self.backbone)
            .field("retention_days", &self.retention_days)
            .field("expires_on", &self.expires_on)
            .field("quota_bytes", &self.quota_bytes)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub servers: Vec<ServerConfig>,
    pub data_dir: PathBuf,
    pub pipeline_depth: usize,
    pub min_connections: usize,
    pub nzb_cache_bytes: u64,
    pub read_ahead_bytes: u64,
    pub read_budget_bytes: usize,
    pub download: DownloadConfig,
}

impl EngineConfig {
    #[must_use]
    pub fn new(data_dir: PathBuf, servers: Vec<ServerConfig>) -> Self {
        Self {
            servers,
            data_dir,
            pipeline_depth: 4,
            min_connections: 0,
            nzb_cache_bytes: 256 << 20,
            read_ahead_bytes: 32 << 20,
            read_budget_bytes: 256 << 20,
            download: DownloadConfig::default(),
        }
    }
}

#[derive(Clone)]
pub struct DownloadConfig {
    pub active_jobs: usize,
    pub free_space_floor: u64,
    pub free_space: Arc<dyn FreeSpace>,
    pub unrar: PathBuf,
    pub sevenzip: PathBuf,
    pub repair_memory_bytes: usize,
    pub straggler_after: Duration,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            active_jobs: 2,
            free_space_floor: 1 << 30,
            free_space: Arc::new(Statvfs),
            unrar: PathBuf::from("unrar"),
            sevenzip: PathBuf::from("7z"),
            repair_memory_bytes: 1 << 30,
            straggler_after: Duration::from_secs(2),
        }
    }
}

impl fmt::Debug for DownloadConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DownloadConfig")
            .field("active_jobs", &self.active_jobs)
            .field("free_space_floor", &self.free_space_floor)
            .field("unrar", &self.unrar)
            .field("sevenzip", &self.sevenzip)
            .field("repair_memory_bytes", &self.repair_memory_bytes)
            .field("straggler_after", &self.straggler_after)
            .finish_non_exhaustive()
    }
}
