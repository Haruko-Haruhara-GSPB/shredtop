//! `shredder status` — show the most recent snapshot from the metrics log.
//!
//! Reads the last line from /var/log/shredder.jsonl and prints a static
//! one-shot table. Use this to check on the running service without
//! opening the live dashboard.

use anyhow::Result;
use chrono::{TimeZone, Utc};

use crate::run::DEFAULT_LOG;

pub fn run() -> Result<()> {
    let content = match std::fs::read_to_string(DEFAULT_LOG) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("No metrics log found at {}.", DEFAULT_LOG);
            eprintln!("Start the service first:  shredder service start");
            return Ok(());
        }
    };

    let line = match content.lines().filter(|l| !l.is_empty()).last() {
        Some(l) => l,
        None => {
            eprintln!("Metrics log is empty — service may just be starting.");
            return Ok(());
        }
    };

    let entry: serde_json::Value = serde_json::from_str(line)?;
    let ts = entry["ts"].as_u64().unwrap_or(0) as i64;
    let dt = Utc.timestamp_opt(ts, 0).single();
    let time_str = dt
        .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "unknown".into());

    let width = 90;
    println!("{:=<width$}", "");
    println!(
        "{:^width$}",
        format!(" SHREDDER STATUS  {} ", time_str)
    );
    println!("{:=<width$}", "");
    println!();
    println!(
        "{:<20}  {:>9}  {:>5}  {:>5}  {:>5}  {}",
        "SOURCE", "SHREDS/s", "COV%", "WIN%", "TXS/s", "LEAD µs (mean)"
    );
    println!("{:-<width$}", "");

    if let Some(sources) = entry["sources"].as_array() {
        for s in sources {
            let name = s["name"].as_str().unwrap_or("?");
            let shreds = s["shreds_per_sec"].as_f64().unwrap_or(0.0);
            let cov = s["coverage_pct"]
                .as_f64()
                .map(|p| format!("{:.0}%", p))
                .unwrap_or_else(|| "—".into());
            let win = s["win_rate_pct"]
                .as_f64()
                .map(|p| format!("{:.0}%", p))
                .unwrap_or_else(|| "—".into());
            let txs = s["txs_per_sec"].as_f64().unwrap_or(0.0);
            let lead = s["lead_time_mean_us"]
                .as_f64()
                .map(|u| format!("{:+.0}", u))
                .unwrap_or_else(|| "—".into());
            println!(
                "{:<20}  {:>9.0}  {:>5}  {:>5}  {:>5.0}  {}",
                name, shreds, cov, win, txs, lead
            );
        }
    }

    println!("{:-<width$}", "");
    println!();
    println!("Log: {}  (shredder service status for service health)", DEFAULT_LOG);

    Ok(())
}
