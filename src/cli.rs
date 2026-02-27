//! CLI definitions for shredder.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(
    name = "shredder",
    version,
    about = "Solana shred feed latency benchmark\n\nMeasures how many milliseconds ahead of confirmed RPC your DoubleZero or Jito ShredStream feed delivers transactions.\n\nQuick start:\n  shredder discover          # detect feeds, write probe.toml\n  shredder service start     # start background collection\n  shredder monitor           # open live dashboard",
    long_about = None
)]
pub struct Cli {
    /// Path to probe.toml config file
    #[clap(long, short, default_value = "probe.toml")]
    pub config: PathBuf,

    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Detect active shred feeds and write probe.toml
    Discover,

    /// Start background data collection as a systemd service
    ///
    /// Run this once after install. Installs the unit file, enables it on
    /// boot, and starts the service immediately. If the service is already
    /// running, shows current status instead.
    Service {
        #[clap(subcommand)]
        action: ServiceAction,
    },

    /// Live dashboard showing feed quality (Ctrl-C closes view, service keeps running)
    ///
    /// Read-only view of the metrics written by `shredder service start`.
    /// Requires the service to be running first.
    Monitor {
        /// Dashboard refresh interval in seconds
        #[clap(long, default_value = "15")]
        interval: u64,
    },

    /// Latest metrics snapshot from the service log (non-interactive)
    Status,

    /// Run a timed benchmark and write a structured JSON report
    Bench {
        /// How many seconds to run the benchmark
        #[clap(long, default_value = "60")]
        duration: u64,

        /// Write JSON report to this file (default: stdout)
        #[clap(long)]
        output: Option<PathBuf>,
    },

    /// Print an example probe.toml to stdout
    Init,

    /// Upgrade shredder to the latest release binary
    Upgrade {
        /// Pull main branch and rebuild from source instead of downloading a release
        #[clap(long)]
        source: bool,
    },

    /// Background data collection daemon (used by the systemd service)
    #[clap(hide = true)]
    Run {
        /// Snapshot interval in seconds
        #[clap(long, default_value = "15")]
        interval: u64,

        /// Path to write metrics log (JSONL)
        #[clap(long, default_value = crate::run::DEFAULT_LOG)]
        log: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ServiceAction {
    /// Install the unit file, enable on boot, and start (run this once to set up)
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
    /// Show systemd service status
    Status,
    /// Enable the service to start on boot
    Enable,
    /// Disable the service from starting on boot
    Disable,
    /// Stop, disable, and remove the unit file
    Uninstall,
}
