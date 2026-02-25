//! `shredder run` — background data collection daemon.
//!
//! Runs the full shred pipeline with no TUI and writes per-source metrics
//! snapshots to a JSONL log file every N seconds. Designed to run under
//! systemd or in a tmux session. Use `shredder status` to query the log,
//! or `shredder service install` to manage via systemd.

use anyhow::Result;
use serde::Serialize;
use shred_ingest::{DecodedTx, FanInSource, SourceMetricsSnapshot};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::ProbeConfig;
use crate::monitor::build_source;

pub const DEFAULT_LOG: &str = "/var/log/shredder.jsonl";

#[derive(Serialize)]
struct LogEntry<'a> {
    ts: u64,
    sources: Vec<SourceSnap<'a>>,
}

#[derive(Serialize)]
struct SourceSnap<'a> {
    name: &'a str,
    shreds_per_sec: f64,
    coverage_pct: Option<f64>,
    win_rate_pct: Option<f64>,
    lead_time_mean_us: Option<f64>,
    lead_time_min_us: Option<i64>,
    lead_time_max_us: Option<i64>,
    lead_time_samples: u64,
    txs_per_sec: f64,
}

pub fn run(config: &ProbeConfig, interval_secs: u64, log_path: PathBuf) -> Result<()> {
    if config.sources.is_empty() {
        anyhow::bail!("no sources configured — run `shredder discover` first");
    }

    eprintln!(
        "shredder run — {} source(s), logging to {} every {}s",
        config.sources.len(),
        log_path.display(),
        interval_secs
    );
    eprintln!("Run `shredder status` to check current metrics.");

    let mut fan_in = FanInSource::new();
    for entry in &config.sources {
        let (source, metrics) = build_source(entry)?;
        fan_in.add_source(source, metrics);
    }

    let (out_tx, out_rx) = crossbeam_channel::bounded::<DecodedTx>(4096);
    let (all_metrics, _handles) = fan_in.start(out_tx);

    std::thread::spawn(move || {
        for _ in out_rx {}
    });

    let interval = Duration::from_secs(interval_secs);
    let mut prev: Vec<SourceMetricsSnapshot> = all_metrics.iter().map(|m| m.snapshot()).collect();
    let mut prev_time = Instant::now();

    loop {
        std::thread::sleep(interval);

        let now = Instant::now();
        let elapsed = now.duration_since(prev_time).as_secs_f64();
        prev_time = now;

        let curr: Vec<SourceMetricsSnapshot> = all_metrics.iter().map(|m| m.snapshot()).collect();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry = LogEntry {
            ts,
            sources: curr
                .iter()
                .zip(prev.iter())
                .map(|(c, p)| make_snap(c, p, elapsed))
                .collect(),
        };

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(file, "{}", line);
            }
        }

        prev = curr;
    }
}

fn make_snap<'a>(
    c: &'a SourceMetricsSnapshot,
    p: &SourceMetricsSnapshot,
    elapsed: f64,
) -> SourceSnap<'a> {
    let shreds_delta = c.shreds_received.saturating_sub(p.shreds_received);
    let txs_delta = c.txs_decoded.saturating_sub(p.txs_decoded);

    let coverage_pct = if c.coverage_shreds_expected > 0 {
        Some((c.coverage_shreds_seen as f64 / c.coverage_shreds_expected as f64 * 100.0).min(100.0))
    } else {
        None
    };

    let win_rate_pct = {
        let total = c.txs_first + c.txs_duplicate;
        if total > 0 {
            Some(c.txs_first as f64 / total as f64 * 100.0)
        } else {
            None
        }
    };

    let (lead_mean, lead_min, lead_max) = if c.lead_time_count > 0 {
        let mean = c.lead_time_sum_us as f64 / c.lead_time_count as f64;
        let min = if c.lead_time_min_us == i64::MAX {
            None
        } else {
            Some(c.lead_time_min_us)
        };
        let max = if c.lead_time_max_us == i64::MIN {
            None
        } else {
            Some(c.lead_time_max_us)
        };
        (Some(mean), min, max)
    } else {
        (None, None, None)
    };

    SourceSnap {
        name: c.name,
        shreds_per_sec: shreds_delta as f64 / elapsed,
        coverage_pct,
        win_rate_pct,
        lead_time_mean_us: lead_mean,
        lead_time_min_us: lead_min,
        lead_time_max_us: lead_max,
        lead_time_samples: c.lead_time_count,
        txs_per_sec: txs_delta as f64 / elapsed,
    }
}
