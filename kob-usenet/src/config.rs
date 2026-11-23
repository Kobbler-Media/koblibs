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
    pub fn retains(&self, posted: i64, now: i64) -> bool {
        self.retention_days
            .is_none_or(|days| now - posted <= i64::from(days) * day::SECONDS)
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
    pub provider_timeout: Duration,
    pub nzb_cache_bytes: u64,
    pub release_cache_bytes: u64,
    pub maintenance_interval: Duration,
    pub read_ahead_bytes: u64,
    pub cache_read_ahead_bytes: u64,
    pub flight_cache_bytes: u64,
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
            provider_timeout: Duration::from_secs(30),
            nzb_cache_bytes: 256 << 20,
            release_cache_bytes: 1 << 30,
            maintenance_interval: Duration::from_secs(30),
            read_ahead_bytes: 32 << 20,
            cache_read_ahead_bytes: 256 << 20,
            flight_cache_bytes: 96 << 20,
            read_budget_bytes: 384 << 20,
            download: DownloadConfig::default(),
        }
    }

    #[must_use]
    pub fn memory_budget_bytes(&self) -> u64 {
        self.read_budget_bytes as u64
            + self.flight_cache_bytes
            + self.download.repair_memory_bytes as u64
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
    pub unpack_stall: Duration,
    pub straggler_after: Duration,
    pub retry_after: Duration,
    pub retry_max: Duration,
    pub retry_attempts: u32,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            active_jobs: 2,
            free_space_floor: 1 << 30,
            free_space: Arc::new(Statvfs),
            unrar: PathBuf::from("unrar"),
            sevenzip: PathBuf::from("7z"),
            repair_memory_bytes: 256 << 20,
            unpack_stall: Duration::from_mins(10),
            straggler_after: Duration::from_secs(2),
            retry_after: Duration::from_mins(1),
            retry_max: Duration::from_mins(30),
            retry_attempts: 12,
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
            .field("unpack_stall", &self.unpack_stall)
            .field("straggler_after", &self.straggler_after)
            .field("retry_after", &self.retry_after)
            .field("retry_max", &self.retry_max)
            .field("retry_attempts", &self.retry_attempts)
            .finish_non_exhaustive()
    }
}
