//! `shredder monitor` — live dashboard reading from the service metrics log.
//!
//! This command is a read-only view. It reads `/var/log/shredder.jsonl` written
//! by `shredder run` / `shredder service start` and redraws the dashboard every
//! N seconds. Ctrl-C closes the view; the background service keeps running.

use anyhow::Result;
use chrono::{TimeZone, Utc};
use libc;
use shred_ingest::{GeyserTxSource, JitoShredstreamSource, RpcTxSource, ShredTxSource, SourceMetrics};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::SourceEntry;
use crate::run::DEFAULT_LOG;

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_sigint(_: libc::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

pub fn run(interval_secs: u64) -> Result<()> {
    // Check that the log file exists and has data before entering the loop
    match std::fs::metadata(DEFAULT_LOG) {
        Err(_) => {
            eprintln!("No metrics data found at {}.", DEFAULT_LOG);
            eprintln!();
            eprintln!("Start the background service first:");
            eprintln!("  shredder service start");
            eprintln!();
            eprintln!("Then run `shredder monitor` again.");
            return Ok(());
        }
        Ok(m) if m.len() == 0 => {
            eprintln!("Service is starting — no data yet. Try again in a few seconds.");
            return Ok(());
        }
        Ok(_) => {}
    }

    RUNNING.store(true, Ordering::SeqCst);
    unsafe { libc::signal(libc::SIGINT, handle_sigint as *const () as libc::sighandler_t) };

    println!("SHREDDER MONITOR  —  Ctrl-C to close  (service keeps running)");
    println!();

    let mut lines_drawn = 0usize;

    while RUNNING.load(Ordering::SeqCst) {
        let snapshot = read_last_entry(DEFAULT_LOG);

        // Overwrite previous dashboard draw
        if lines_drawn > 0 {
            print!("\x1b[{}A\x1b[0J", lines_drawn);
        }

        lines_drawn = match snapshot {
            Some(entry) => draw_dashboard(&entry),
            None => {
                let line = "Waiting for first snapshot...";
                println!("{}", line);
                1
            }
        };
        std::io::stdout().flush().ok();

        // Sleep in small increments so Ctrl-C is responsive
        let mut waited = 0u64;
        while waited < interval_secs && RUNNING.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            waited += 1;
        }
    }

    println!();
    println!("View closed.  Service is still running in the background.");
    println!("  shredder status  — check metrics any time");

    Ok(())
}

fn read_last_entry(path: &str) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(path).ok()?;
    let line = content.lines().filter(|l| !l.is_empty()).last()?;
    serde_json::from_str(line).ok()
}

fn draw_dashboard(entry: &serde_json::Value) -> usize {
    const W: usize = 100;
    let mut out: Vec<String> = Vec::new();

    // Timestamp from log entry
    let ts = entry["ts"].as_u64().unwrap_or(0) as i64;
    let time_str = Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "—".into());

    // Header
    out.push("=".repeat(W));
    out.push(format!("{:^W$}", format!("  SHREDDER FEED QUALITY  {}  ", time_str)));
    out.push("=".repeat(W));
    out.push(String::new());

    // Column headers
    out.push(format!(
        "{:<20}  {:>9}  {:>5}  {:>6}  {:>6}  {:>9}  {:>9}  {:>9}  {:>9}",
        "SOURCE", "SHREDS/s", "COV%", "TXS/s", "BEAT%", "LEAD avg", "LEAD p50", "LEAD p95", "LEAD p99",
    ));
    out.push("-".repeat(W));

    let mut edge_lines: Vec<String> = Vec::new();
    let mut has_rpc = false;

    if let Some(sources) = entry["sources"].as_array() {
        for s in sources {
            let name = s["name"].as_str().unwrap_or("?");
            let is_rpc = s["is_rpc"].as_bool().unwrap_or(false);
            if is_rpc {
                has_rpc = true;
            }

            let shreds_str = if is_rpc {
                "—".into()
            } else {
                format!("{:.0}", s["shreds_per_sec"].as_f64().unwrap_or(0.0))
            };

            let cov_str = s["coverage_pct"]
                .as_f64()
                .map(|p| format!("{:.0}%", p.min(100.0)))
                .unwrap_or_else(|| "—".into());

            let beat_str = if is_rpc {
                "—".into()
            } else {
                s["beat_rpc_pct"]
                    .as_f64()
                    .map(|p| format!("{:.0}%", p))
                    .unwrap_or_else(|| "—".into())
            };

            let txs_str = format!("{:.0}", s["txs_per_sec"].as_f64().unwrap_or(0.0));

            let (avg_str, p50_str, p95_str, p99_str) = if is_rpc {
                ("baseline".into(), "—".into(), "—".into(), "—".into())
            } else if let Some(mean_us) = s["lead_time_mean_us"].as_f64() {
                let avg = format!("{:+.1}ms", mean_us / 1000.0);
                let p50 = s["lead_time_p50_us"].as_f64()
                    .map(|v| format!("{:+.1}ms", v / 1000.0))
                    .unwrap_or_else(|| "—".into());
                let p95 = s["lead_time_p95_us"].as_f64()
                    .map(|v| format!("{:+.1}ms", v / 1000.0))
                    .unwrap_or_else(|| "—".into());
                let p99 = s["lead_time_p99_us"].as_f64()
                    .map(|v| format!("{:+.1}ms", v / 1000.0))
                    .unwrap_or_else(|| "—".into());
                (avg, p50, p95, p99)
            } else {
                ("—".into(), "—".into(), "—".into(), "—".into())
            };

            out.push(format!(
                "{:<20}  {:>9}  {:>5}  {:>6}  {:>6}  {:>9}  {:>9}  {:>9}  {:>9}",
                name, shreds_str, cov_str, txs_str, beat_str, avg_str, p50_str, p95_str, p99_str,
            ));

            // Edge assessment for shred sources
            if !is_rpc {
                if let Some(mean_us) = s["lead_time_mean_us"].as_f64() {
                    let mean_ms = mean_us / 1000.0;
                    let samples = s["lead_time_samples"].as_u64().unwrap_or(0);
                    let (label, symbol) = if mean_us > 1_000.0 {
                        ("AHEAD of RPC", "\x1b[32m✓\x1b[0m")
                    } else if mean_us > 0.0 {
                        ("marginally ahead", "\x1b[33m~\x1b[0m")
                    } else if mean_us > -5_000.0 {
                        ("BEHIND RPC", "\x1b[31m⚠\x1b[0m")
                    } else {
                        ("BADLY BEHIND RPC", "\x1b[31m✗\x1b[0m")
                    };
                    edge_lines.push(format!(
                        "  {}  {:<20} {}  by {:.2}ms avg  ({} samples)",
                        symbol, name, label, mean_ms.abs(), samples,
                    ));
                }
            }
        }
    }

    out.push("-".repeat(W));
    out.push(String::new());

    // Edge assessment
    out.push("EDGE ASSESSMENT:".into());
    if edge_lines.is_empty() {
        if !has_rpc {
            out.push("  Add an rpc source to probe.toml to enable lead-time measurement.".into());
        } else {
            out.push("  Warming up — lead times appear once transactions match across feeds.".into());
        }
    } else {
        for line in &edge_lines {
            out.push(line.clone());
        }
    }

    out.push(String::new());
    out.push("-".repeat(W));
    out.push(
        "COV% = block shreds received  BEAT% = % of matched txs where feed beat RPC  \
         LEAD = ms before RPC confirms  p50/p95/p99 = percentiles"
            .into(),
    );

    let count = out.len();
    for line in out {
        println!("{}", line);
    }
    count
}

// ---------------------------------------------------------------------------
// Source construction — used by run.rs
// ---------------------------------------------------------------------------

pub fn build_source(
    entry: &SourceEntry,
) -> Result<(Box<dyn shred_ingest::TxSource>, Arc<SourceMetrics>)> {
    let name: &'static str = Box::leak(entry.name.clone().into_boxed_str());
    // rpc and geyser are baseline sources; shred and jito-grpc are shred-tier feeds.
    let is_rpc = matches!(entry.source_type.as_str(), "rpc" | "geyser");
    let metrics = SourceMetrics::new(name, is_rpc);

    let source: Box<dyn shred_ingest::TxSource> = match entry.source_type.as_str() {
        "shred" => {
            let multicast_addr = entry
                .multicast_addr
                .clone()
                .ok_or_else(|| anyhow::anyhow!("source '{}': missing multicast_addr", name))?;
            let port = entry.port.unwrap_or(20001);
            let interface = entry
                .interface
                .clone()
                .unwrap_or_else(|| "doublezero1".into());
            Box::new(ShredTxSource {
                name,
                multicast_addr,
                port,
                interface,
                pin_recv_core: entry.pin_recv_core,
                pin_decode_core: entry.pin_decode_core,
                shred_version: entry.shred_version,
            })
        }
        "rpc" => {
            let url = entry
                .url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:8899".into());
            Box::new(RpcTxSource { url, pin_core: entry.pin_recv_core })
        }
        "geyser" => {
            let url = entry
                .url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("source '{}': missing url for geyser source", name))?;
            Box::new(GeyserTxSource { name, url, x_token: entry.x_token.clone() })
        }
        "jito-grpc" => {
            let url = entry
                .url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("source '{}': missing url for jito-grpc source", name))?;
            Box::new(JitoShredstreamSource { name, url })
        }
        other => {
            anyhow::bail!("unknown source_type '{}' for source '{}'", other, name);
        }
    };

    Ok((source, metrics))
}
