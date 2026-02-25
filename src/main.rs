//! shredder — Solana shred feed latency benchmark.
//!
//! Measures the latency advantage of DoubleZero / Jito ShredStream raw shred
//! feeds over confirmed-block RPC polling. Run `shredder --help` for usage.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod bench;
mod cli;
mod config;
mod discover;
mod monitor;

use cli::{Cli, Commands};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("warn".parse()?))
        .init();

    let cli = Cli::parse();

    // Load config (except for commands that don't need it)
    let config = match &cli.command {
        Commands::Init | Commands::Upgrade => None,
        _ => {
            if !cli.config.exists() {
                std::fs::write(&cli.config, b"")?;
                eprintln!(
                    "Created '{}' — run `shredder discover` to populate it.",
                    cli.config.display()
                );
            }
            Some(config::ProbeConfig::load(&cli.config)?)
        }
    };

    match cli.command {
        Commands::Init => {
            let example = config::ProbeConfig::default_example();
            print!("{}", toml::to_string_pretty(&example)?);
        }
        Commands::Upgrade => {
            let status = std::process::Command::new("cargo")
                .args([
                    "install",
                    "--git",
                    "https://github.com/Haruko-Haruhara-GSPB/shred-probe.git",
                    "--force",
                ])
                .status()?;
            if !status.success() {
                anyhow::bail!("upgrade failed");
            }
        }
        Commands::Discover => {
            discover::run(config.as_ref().unwrap(), &cli.config)?;
        }
        Commands::Monitor { interval } => {
            monitor::run(config.as_ref().unwrap(), interval)?;
        }
        Commands::Bench { duration, output } => {
            bench::run(config.as_ref().unwrap(), duration, output)?;
        }
    }

    Ok(())
}
