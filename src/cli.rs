//! CLI definitions for shred-probe.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(
    name = "shred-probe",
    version,
    about = "Solana shred feed latency benchmark\n\nMeasure your edge: how many milliseconds ahead of confirmed RPC do DoubleZero or Jito ShredStream feeds deliver transactions?",
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
    /// Show available multicast groups, active memberships, and configured sources
    Discover,

    /// Live-updating feed quality dashboard (Ctrl-C to stop)
    Monitor {
        /// Dashboard refresh interval in seconds
        #[clap(long, default_value = "5")]
        interval: u64,
    },

    /// Run a timed benchmark and output a structured report
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

    /// Upgrade shred-probe to the latest version from GitHub
    Upgrade,
}
