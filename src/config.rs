//! `probe.toml` configuration for shredder.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level probe configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbeConfig {
    #[serde(default)]
    pub sources: Vec<SourceEntry>,
    /// Optional list of program or account pubkeys (base58). When non-empty, only
    /// transactions whose static account keys include at least one of these pubkeys
    /// are included in lead-time statistics. Applies to shred-tier sources only;
    /// RPC-tier sources (rpc, geyser, jito-grpc) are always exempt.
    #[serde(default)]
    pub filter_programs: Vec<String>,
    /// Raw shred capture configuration. Omit to disable capture.
    #[serde(default)]
    pub capture: Option<CaptureConfig>,
}

/// Configuration for the always-on ring-buffer capture subsystem.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CaptureConfig {
    /// Enable capture.
    #[serde(default = "CaptureConfig::default_enabled")]
    pub enabled: bool,
    /// Output format: "pcap" (Wireshark), "csv", or "jsonl".
    #[serde(default = "CaptureConfig::default_format")]
    pub format: String,
    /// Directory to write capture files into.
    #[serde(default = "CaptureConfig::default_output_dir")]
    pub output_dir: String,
    /// Rotate to a new file after this many megabytes.
    #[serde(default = "CaptureConfig::default_rotate_mb")]
    pub rotate_mb: u64,
    /// Keep at most this many files; delete oldest on overflow.
    #[serde(default = "CaptureConfig::default_ring_files")]
    pub ring_files: usize,
}

impl CaptureConfig {
    fn default_enabled() -> bool { true }
    fn default_format() -> String { "pcap".into() }
    fn default_output_dir() -> String { "/var/log/shredder-capture".into() }
    fn default_rotate_mb() -> u64 { 500 }
    fn default_ring_files() -> usize { 20 }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            format: Self::default_format(),
            output_dir: Self::default_output_dir(),
            rotate_mb: Self::default_rotate_mb(),
            ring_files: Self::default_ring_files(),
        }
    }
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
    /// UDP port (shred only; bebop=7733, jito-shredstream=20001)
    pub port: Option<u16>,
    /// Network interface for multicast (shred only, e.g. "doublezero1")
    pub interface: Option<String>,
    /// RPC endpoint URL (rpc or geyser)
    pub url: Option<String>,
    /// Authentication token sent as `x-token` header (geyser only)
    pub x_token: Option<String>,
    /// CPU core to pin receiver thread to (optional)
    pub pin_recv_core: Option<usize>,
    /// CPU core to pin decoder thread to (optional)
    pub pin_decode_core: Option<usize>,
    /// Only accept shreds with this version (bytes 77-78). Silently drops mismatches.
    /// Useful during forks or network upgrades. Omit to accept all versions.
    #[serde(default)]
    pub shred_version: Option<u16>,
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
            filter_programs: Vec::new(),
            capture: None,
            sources: vec![
                SourceEntry {
                    name: "bebop".into(),
                    source_type: "shred".into(),
                    multicast_addr: Some("233.84.178.1".into()),
                    port: Some(7733),
                    interface: Some("doublezero1".into()),
                    url: None,
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                    shred_version: None,
                },
                SourceEntry {
                    name: "jito-shredstream".into(),
                    source_type: "shred".into(),
                    multicast_addr: Some("233.84.178.2".into()),
                    port: Some(20001),
                    interface: Some("doublezero1".into()),
                    url: None,
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                    shred_version: None,
                },
                SourceEntry {
                    name: "rpc".into(),
                    source_type: "rpc".into(),
                    multicast_addr: None,
                    port: None,
                    interface: None,
                    url: Some("http://127.0.0.1:8899".into()),
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                    shred_version: None,
                },
            ],
        }
    }
}
