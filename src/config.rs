//! `probe.toml` configuration for shredder.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level probe configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbeConfig {
    #[serde(default)]
    pub sources: Vec<SourceEntry>,
}

/// One data source (shred feed or RPC endpoint).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceEntry {
    /// Human-readable name shown in the dashboard (e.g. "bebop", "jito-shredstream", "rpc")
    pub name: String,
    /// Source type: "shred" or "rpc"
    #[serde(rename = "type")]
    pub source_type: String,
    /// Multicast group IP (shred only)
    pub multicast_addr: Option<String>,
    /// UDP port (shred only, default 20001)
    pub port: Option<u16>,
    /// Network interface for multicast (shred only, e.g. "doublezero1")
    pub interface: Option<String>,
    /// RPC endpoint URL (rpc only)
    pub url: Option<String>,
    /// CPU core to pin receiver thread to (optional)
    pub pin_recv_core: Option<usize>,
    /// CPU core to pin decoder thread to (optional)
    pub pin_decode_core: Option<usize>,
}

impl ProbeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let cfg: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config file: {}", path.display()))?;
        Ok(cfg)
    }

    /// Returns a default config that matches the standard DoubleZero + RPC setup.
    pub fn default_example() -> Self {
        Self {
            sources: vec![
                SourceEntry {
                    name: "bebop".into(),
                    source_type: "shred".into(),
                    multicast_addr: Some("233.84.178.1".into()),
                    port: Some(20001),
                    interface: Some("doublezero1".into()),
                    url: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                },
                SourceEntry {
                    name: "jito-shredstream".into(),
                    source_type: "shred".into(),
                    multicast_addr: Some("233.84.178.2".into()),
                    port: Some(20001),
                    interface: Some("doublezero1".into()),
                    url: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                },
                SourceEntry {
                    name: "rpc".into(),
                    source_type: "rpc".into(),
                    multicast_addr: None,
                    port: None,
                    interface: None,
                    url: Some("http://127.0.0.1:8899".into()),
                    pin_recv_core: None,
                    pin_decode_core: None,
                },
            ],
        }
    }
}
