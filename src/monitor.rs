//! `shredder monitor` — live-updating feed quality dashboard.
//!
//! Starts all configured sources and prints a rolling quality report every
//! `interval` seconds. Uses ANSI escape codes to clear and redraw the terminal
//! in-place for a dashboard effect. Press Ctrl-C to stop.

use anyhow::Result;
use chrono::Local;
use libc;
use shred_ingest::{
    DecodedTx, FanInSource, RpcTxSource, ShredTxSource, SourceMetrics, SourceMetricsSnapshot,
};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_sigint(_: libc::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

use crate::config::{ProbeConfig, SourceEntry};

pub fn run(config: &ProbeConfig, interval_secs: u64) -> Result<()> {
    if config.sources.is_empty() {
        anyhow::bail!(
            "no sources configured — run `shredder init > probe.toml` to create a config"
        );
    }

    let mut fan_in = FanInSource::new();

    for entry in &config.sources {
        let (source, metrics) = build_source(entry)?;
        fan_in.add_source(source, metrics);
    }

    let (out_tx, out_rx) = crossbeam_channel::bounded::<DecodedTx>(4096);
    let (all_metrics, _handles) = fan_in.start(out_tx);

    // Drain thread — we only care about timing metadata, not the txs themselves
    std::thread::spawn(move || {
        for _ in out_rx {}
    });

    // Enter alternate screen buffer — preserves terminal history so Ctrl-C
    // returns the user to whatever was on screen before (e.g. discover output).
    print!("\x1b[?1049h");
    std::io::stdout().flush().ok();

    // Restore screen on Ctrl-C
    RUNNING.store(true, Ordering::SeqCst);
    unsafe { libc::signal(libc::SIGINT, handle_sigint as *const () as libc::sighandler_t) };

    let interval = Duration::from_secs(interval_secs);
    let mut prev_snapshots: Vec<SourceMetricsSnapshot> =
        all_metrics.iter().map(|m| m.snapshot()).collect();
    let mut prev_time = Instant::now();

    while RUNNING.load(Ordering::SeqCst) {
        std::thread::sleep(interval);

        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }

        let now = Instant::now();
        let elapsed_secs = now.duration_since(prev_time).as_secs_f64();
        prev_time = now;

        let curr_snapshots: Vec<SourceMetricsSnapshot> =
            all_metrics.iter().map(|m| m.snapshot()).collect();

        print!("\x1b[H\x1b[2J");
        print_dashboard(&curr_snapshots, &prev_snapshots, elapsed_secs);
        std::io::stdout().flush().ok();

        prev_snapshots = curr_snapshots;
    }

    // Exit alternate screen — terminal restores previous content
    print!("\x1b[?1049l");
    std::io::stdout().flush().ok();

    Ok(())
}

fn print_dashboard(
    curr: &[SourceMetricsSnapshot],
    prev: &[SourceMetricsSnapshot],
    elapsed_secs: f64,
) {
    let now = Local::now();
    let width = 90;

    // Header
    let title = format!(
        " SHRED-PROBE FEED QUALITY DASHBOARD  {} ",
        now.format("%Y-%m-%d %H:%M:%S")
    );
    println!("{:=<width$}", "");
    println!("{:^width$}", title);
    println!("{:=<width$}", "");
    println!();

    // Column headers
    println!(
        "{:<20}  {:>9}  {:>5}  {:>5}  {:>5}  {:>7}  {}",
        "SOURCE", "SHREDS/s", "COV%", "WIN%", "TXS/s", "FEC-REC", "LEAD µs (mean / min / max)"
    );
    println!("{:-<width$}", "");

    let mut edge_lines: Vec<String> = Vec::new();
    let mut has_rpc = false;

    for (c, p) in curr.iter().zip(prev.iter()) {
        let shreds_per_s = delta_u64(c.shreds_received, p.shreds_received) as f64 / elapsed_secs;
        let txs_per_s = delta_u64(c.txs_decoded, p.txs_decoded) as f64 / elapsed_secs;
        let fec_delta = delta_u64(c.fec_recovered_shreds, p.fec_recovered_shreds);

        let cov_pct = if c.coverage_shreds_expected > 0 {
            format!(
                "{:.0}%",
                c.coverage_shreds_seen as f64 / c.coverage_shreds_expected as f64 * 100.0
            )
        } else {
            "—".into()
        };

        let win_pct = {
            let first = c.txs_first;
            let dup = c.txs_duplicate;
            if first + dup > 0 {
                format!("{:.0}%", first as f64 / (first + dup) as f64 * 100.0)
            } else {
                "—".into()
            }
        };

        let lead_str = if c.lead_time_count > 0 {
            let mean = c.lead_time_sum_us as f64 / c.lead_time_count as f64;
            let min_us = c.lead_time_min_us;
            let max_us = c.lead_time_max_us;
            format!("{:+.0} / {:+} / {:+}", mean, min_us, max_us)
        } else {
            "—".into()
        };

        let is_rpc = c.name == "rpc";
        if is_rpc {
            has_rpc = true;
        }

        println!(
            "{:<20}  {:>9.0}  {:>5}  {:>5}  {:>5.0}  {:>7}  {}",
            c.name, shreds_per_s, cov_pct, win_pct, txs_per_s, fec_delta, lead_str,
        );

        // Prepare edge assessment line for non-RPC sources with lead data
        if !is_rpc && c.lead_time_count > 0 {
            let mean_us = c.lead_time_sum_us as f64 / c.lead_time_count as f64;
            let mean_ms = mean_us / 1000.0;
            let (status, symbol) = if mean_us > 1_000.0 {
                ("AHEAD of RPC", "\x1b[32m✓\x1b[0m") // green
            } else if mean_us > 0.0 {
                ("marginally ahead", "\x1b[33m~\x1b[0m") // yellow
            } else if mean_us > -5_000.0 {
                ("BEHIND RPC", "\x1b[31m⚠\x1b[0m") // red
            } else {
                ("BADLY BEHIND RPC", "\x1b[31m✗\x1b[0m") // red
            };
            edge_lines.push(format!(
                "  {} {}  {} by avg {:.2}ms  (n={})",
                symbol,
                c.name,
                status,
                mean_ms.abs(),
                c.lead_time_count,
            ));
        }
    }

    println!("{:-<width$}", "");
    println!();

    // Edge assessment
    if !edge_lines.is_empty() {
        println!("EDGE:");
        for line in &edge_lines {
            println!("{}", line);
        }
    } else if has_rpc {
        println!("EDGE: no lead-time data yet — waiting for matching transactions from shred + RPC");
    } else {
        println!("EDGE: add an RPC source to probe.toml to enable lead-time measurement");
    }

    println!();
    println!(
        "Window: {:.0}s  |  Press Ctrl-C to stop",
        elapsed_secs
    );
}

fn delta_u64(curr: u64, prev: u64) -> u64 {
    curr.saturating_sub(prev)
}

// ---------------------------------------------------------------------------
// Source construction from config
// ---------------------------------------------------------------------------

pub fn build_source(
    entry: &SourceEntry,
) -> Result<(Box<dyn shred_ingest::TxSource>, Arc<SourceMetrics>)> {
    // Leak the name string so we get a &'static str
    let name: &'static str = Box::leak(entry.name.clone().into_boxed_str());
    let metrics = SourceMetrics::new(name);

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
            })
        }
        "rpc" => {
            let url = entry
                .url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:8899".into());
            Box::new(RpcTxSource { url, pin_core: entry.pin_recv_core })
        }
        other => {
            anyhow::bail!("unknown source_type '{}' for source '{}'", other, name);
        }
    };

    Ok((source, metrics))
}
