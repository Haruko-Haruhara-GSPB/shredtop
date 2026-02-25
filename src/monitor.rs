//! `shredder monitor` — live-updating feed quality dashboard.
//!
//! Redraws the dashboard in-place using cursor-up escape codes, so the
//! terminal history above the dashboard is never cleared.  Press Ctrl-C
//! to stop; the final snapshot stays on screen.

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

    // Drain decoded transactions — we only need the timing metadata
    std::thread::spawn(move || {
        for _ in out_rx {}
    });

    RUNNING.store(true, Ordering::SeqCst);
    unsafe { libc::signal(libc::SIGINT, handle_sigint as *const () as libc::sighandler_t) };

    let source_names: Vec<&str> = config.sources.iter().map(|s| s.name.as_str()).collect();
    println!(
        "Monitoring: {}",
        source_names.join("  |  ")
    );
    println!("Refresh: every {}s  —  Ctrl-C to stop, dashboard stays on screen", interval_secs);
    println!();

    let interval = Duration::from_secs(interval_secs);
    let start_time = Instant::now();
    let mut prev_snapshots: Vec<SourceMetricsSnapshot> =
        all_metrics.iter().map(|m| m.snapshot()).collect();
    let mut prev_time = Instant::now();

    // First draw — warming up, no delta data yet
    let mut lines_drawn = print_dashboard(
        &prev_snapshots,
        &prev_snapshots,
        interval_secs as f64,
        0.0,
        true,
    );
    std::io::stdout().flush().ok();

    while RUNNING.load(Ordering::SeqCst) {
        std::thread::sleep(interval);
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }

        let now = Instant::now();
        let elapsed_secs = now.duration_since(prev_time).as_secs_f64();
        prev_time = now;
        let running_secs = start_time.elapsed().as_secs_f64();

        let curr_snapshots: Vec<SourceMetricsSnapshot> =
            all_metrics.iter().map(|m| m.snapshot()).collect();

        // Move cursor up to overwrite the previous dashboard without touching
        // anything above it in the terminal history
        print!("\x1b[{}A\x1b[0J", lines_drawn);
        lines_drawn = print_dashboard(
            &curr_snapshots,
            &prev_snapshots,
            elapsed_secs,
            running_secs,
            false,
        );
        std::io::stdout().flush().ok();

        prev_snapshots = curr_snapshots;
    }

    println!();
    println!("Stopped.  To collect metrics in the background:");
    println!("  shredder run     — start background data collection");
    println!("  shredder status  — view latest snapshot at any time");

    Ok(())
}

// ---------------------------------------------------------------------------
// Dashboard renderer — returns the number of lines printed so the caller
// can move the cursor back up for the next redraw.
// ---------------------------------------------------------------------------

fn print_dashboard(
    curr: &[SourceMetricsSnapshot],
    prev: &[SourceMetricsSnapshot],
    elapsed_secs: f64,
    running_secs: f64,
    warming_up: bool,
) -> usize {
    const W: usize = 92;
    let now = Local::now();

    // Collect output into a Vec<String> so we can return an accurate line count
    let mut out: Vec<String> = Vec::new();

    // ── Header ────────────────────────────────────────────────────────────
    let title = format!(
        "  SHREDDER FEED QUALITY  {}  (running {})",
        now.format("%Y-%m-%d %H:%M:%S"),
        fmt_duration(running_secs),
    );
    out.push("=".repeat(W));
    out.push(format!("{:^W$}", title));
    out.push("=".repeat(W));
    out.push(String::new());

    // ── Column headers ────────────────────────────────────────────────────
    out.push(format!(
        "{:<20}  {:>9}  {:>5}  {:>5}  {:>6}  {:>7}  {}",
        "SOURCE", "SHREDS/s", "COV%", "WIN%", "TXS/s", "FEC-REC", "LEAD ms (avg / min / max)"
    ));
    out.push("-".repeat(W));

    // ── Per-source rows ───────────────────────────────────────────────────
    let mut edge_lines: Vec<String> = Vec::new();
    let mut has_rpc = false;
    let mut any_lead_data = false;

    for (c, p) in curr.iter().zip(prev.iter()) {
        let is_rpc = c.name == "rpc";
        if is_rpc {
            has_rpc = true;
        }

        let shreds_per_s =
            delta_u64(c.shreds_received, p.shreds_received) as f64 / elapsed_secs;
        let txs_per_s = delta_u64(c.txs_decoded, p.txs_decoded) as f64 / elapsed_secs;
        let fec_delta = delta_u64(c.fec_recovered_shreds, p.fec_recovered_shreds);

        let shreds_str = if is_rpc {
            "—".into()
        } else {
            format!("{:.0}", shreds_per_s)
        };

        let cov_str = if is_rpc {
            "—".into()
        } else if c.coverage_shreds_expected > 0 {
            let pct = (c.coverage_shreds_seen as f64 / c.coverage_shreds_expected as f64
                * 100.0)
                .min(100.0);
            format!("{:.0}%", pct)
        } else {
            "—".into()
        };

        let win_str = {
            let total = c.txs_first + c.txs_duplicate;
            if total > 0 {
                format!("{:.0}%", c.txs_first as f64 / total as f64 * 100.0)
            } else {
                "—".into()
            }
        };

        let fec_str = if is_rpc {
            "—".into()
        } else {
            format!("{}", fec_delta)
        };

        let lead_str = if is_rpc {
            "  baseline".into()
        } else if c.lead_time_count > 0 {
            any_lead_data = true;
            let avg_ms = c.lead_time_sum_us as f64 / c.lead_time_count as f64 / 1000.0;
            let min_ms = c.lead_time_min_us as f64 / 1000.0;
            let max_ms = c.lead_time_max_us as f64 / 1000.0;
            format!("{:+.2} / {:+.2} / {:+.2}", avg_ms, min_ms, max_ms)
        } else {
            "  —".into()
        };

        out.push(format!(
            "{:<20}  {:>9}  {:>5}  {:>5}  {:>6.0}  {:>7}  {}",
            c.name, shreds_str, cov_str, win_str, txs_per_s, fec_str, lead_str,
        ));

        // Prepare edge assessment for non-RPC sources that have lead data
        if !is_rpc && c.lead_time_count > 0 {
            let mean_us = c.lead_time_sum_us as f64 / c.lead_time_count as f64;
            let mean_ms = mean_us / 1000.0;
            let (label, symbol) = if mean_us > 1_000.0 {
                ("AHEAD of RPC", "\x1b[32m✓\x1b[0m")
            } else if mean_us > 0.0 {
                ("marginally ahead of RPC", "\x1b[33m~\x1b[0m")
            } else if mean_us > -5_000.0 {
                ("BEHIND RPC", "\x1b[31m⚠\x1b[0m")
            } else {
                ("BADLY BEHIND RPC", "\x1b[31m✗\x1b[0m")
            };
            edge_lines.push(format!(
                "  {}  {:<20} {}  by {:.2}ms avg  ({} samples)",
                symbol,
                c.name,
                label,
                mean_ms.abs(),
                c.lead_time_count,
            ));
        }
    }

    out.push("-".repeat(W));
    out.push(String::new());

    // ── Edge assessment ───────────────────────────────────────────────────
    out.push("EDGE ASSESSMENT:".into());
    if warming_up || (!any_lead_data && !edge_lines.is_empty()) || (edge_lines.is_empty() && !has_rpc) {
        if !has_rpc {
            out.push("  Add an rpc source to probe.toml to enable lead-time measurement.".into());
        } else {
            out.push("  Warming up — waiting for transactions to arrive on both shred and RPC feeds.".into());
            out.push("  Lead times will appear here within ~30 seconds.".into());
        }
    } else if edge_lines.is_empty() && has_rpc {
        out.push("  No lead-time data yet — waiting for matching transactions across feeds.".into());
    } else {
        for line in &edge_lines {
            out.push(line.clone());
        }
    }

    out.push(String::new());
    out.push("-".repeat(W));

    // ── Legend ────────────────────────────────────────────────────────────
    out.push(
        "SHREDS/s  packets/sec from feed     \
         COV%  block shreds received     \
         WIN%  txs seen first"
            .into(),
    );
    out.push(
        "FEC-REC   shreds rebuilt by Reed-Solomon     \
         LEAD  milliseconds before RPC confirmation"
            .into(),
    );

    // Print and return count
    let count = out.len();
    for line in out {
        println!("{}", line);
    }
    count
}

fn fmt_duration(secs: f64) -> String {
    let s = secs as u64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
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
