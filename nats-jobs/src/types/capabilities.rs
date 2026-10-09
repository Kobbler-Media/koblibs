use serde::{Deserialize, Serialize};

/// Capability flags for indexer handlers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexerCapabilities {
    #[serde(default)]
    pub supports_search: bool,
    #[serde(default)]
    pub supports_rss: bool,
    #[serde(default)]
    pub supports_newznab: bool,
    #[serde(default)]
    pub supports_torznab: bool,
    #[serde(default)]
    pub supports_categories: bool,
    #[serde(default)]
    pub protocols: Vec<String>,
}

/// Capability flags for download clients.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DownloadClientCapabilities {
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub supports_magnet: bool,
    #[serde(default)]
    pub supports_nzb: bool,
    #[serde(default)]
    pub supports_labels: bool,
    #[serde(default)]
    pub supports_delete: bool,
}
