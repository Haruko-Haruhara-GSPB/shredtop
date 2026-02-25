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

    let width = 96;
    println!("{:=<width$}", "");
    println!(
        "{:^width$}",
        format!(" SHREDDER STATUS  {} ", time_str)
    );
    println!("{:=<width$}", "");
    println!();
    println!(
        "{:<20}  {:>9}  {:>5}  {:>6}  {:>6}  {:>9}  {:>9}  {:>9}",
        "SOURCE", "SHREDS/s", "COV%", "TXS/s", "BEAT%", "LEAD avg", "LEAD min", "LEAD max",
    );
    println!("{:-<width$}", "");

    if let Some(sources) = entry["sources"].as_array() {
        for s in sources {
            let name = s["name"].as_str().unwrap_or("?");
            let is_rpc = name == "rpc";

            let shreds_str = if is_rpc {
                "—".into()
            } else {
                format!("{:.0}", s["shreds_per_sec"].as_f64().unwrap_or(0.0))
            };
            let cov = if is_rpc {
                "—".into()
            } else {
                s["coverage_pct"]
                    .as_f64()
                    .map(|p| format!("{:.0}%", p.min(100.0)))
                    .unwrap_or_else(|| "—".into())
            };
            let txs = s["txs_per_sec"].as_f64().unwrap_or(0.0);
            let beat = if is_rpc {
                "—".into()
            } else {
                s["beat_rpc_pct"]
                    .as_f64()
                    .map(|p| format!("{:.0}%", p))
                    .unwrap_or_else(|| "—".into())
            };
            let (avg_str, min_str, max_str) = if is_rpc {
                ("baseline".into(), "—".into(), "—".into())
            } else if let Some(mean_us) = s["lead_time_mean_us"].as_f64() {
                let avg = format!("{:+.1}ms", mean_us / 1000.0);
                let min = s["lead_time_min_us"].as_f64()
                    .map(|v| format!("{:+.1}ms", v / 1000.0))
                    .unwrap_or_else(|| "—".into());
                let max = s["lead_time_max_us"].as_f64()
                    .map(|v| format!("{:+.1}ms", v / 1000.0))
                    .unwrap_or_else(|| "—".into());
                (avg, min, max)
            } else {
                ("—".into(), "—".into(), "—".into())
            };

            println!(
                "{:<20}  {:>9}  {:>5}  {:>6.0}  {:>6}  {:>9}  {:>9}  {:>9}",
                name, shreds_str, cov, txs, beat, avg_str, min_str, max_str,
            );
        }
    }

    println!("{:-<width$}", "");
    println!();
    println!("Log: {}  (shredder service status for service health)", DEFAULT_LOG);

    Ok(())
}
