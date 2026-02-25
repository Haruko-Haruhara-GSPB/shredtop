//! `shredder bench` — timed benchmark with structured JSON output.
//!
//! Runs all configured sources for a fixed duration, then emits a JSON report
//! with per-source statistics including lead-time histogram, win rate, FEC recovery,
//! and coverage percentage.

use anyhow::Result;
use serde::Serialize;
use shred_ingest::{DecodedTx, FanInSource, SourceMetricsSnapshot};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::ProbeConfig;
use crate::monitor::build_source;

#[derive(Debug, Serialize)]
pub struct BenchReport {
    pub duration_secs: u64,
    pub sources: Vec<SourceReport>,
}

#[derive(Debug, Serialize)]
pub struct SourceReport {
    pub name: String,
    pub shreds_received: u64,
    pub shreds_per_sec: f64,
    pub bytes_received_mb: f64,
    pub shreds_dropped: u64,
    pub slots_attempted: u64,
    pub slots_complete: u64,
    pub slots_partial: u64,
    pub slots_dropped: u64,
    pub coverage_pct: Option<f64>,
    pub fec_recovered_shreds: u64,
    pub txs_decoded: u64,
    pub txs_per_sec: f64,
    pub win_rate_pct: Option<f64>,
    pub lead_time_mean_us: Option<f64>,
    pub lead_time_min_us: Option<i64>,
    pub lead_time_max_us: Option<i64>,
    pub lead_time_samples: u64,
}

pub fn run(config: &ProbeConfig, duration_secs: u64, output: Option<PathBuf>) -> Result<()> {
    if config.sources.is_empty() {
        anyhow::bail!(
            "no sources configured — run `shredder init > probe.toml` to create a config"
        );
    }

    eprintln!(
        "shredder bench — running for {}s with {} source(s)...",
        duration_secs,
        config.sources.len()
    );

    let mut fan_in = FanInSource::new();

    for entry in &config.sources {
        let (source, metrics) = build_source(entry)?;
        fan_in.add_source(source, metrics);
    }

    let (out_tx, out_rx) = crossbeam_channel::bounded::<DecodedTx>(4096);
    let (all_metrics, _handles) = fan_in.start(out_tx);

    // Drain thread
    std::thread::spawn(move || {
        for _ in out_rx {}
    });

    let start = Instant::now();
    let target = Duration::from_secs(duration_secs);

    // Progress indicator every 10s
    let mut next_tick = 10u64;
    while start.elapsed() < target {
        std::thread::sleep(Duration::from_secs(1));
        let elapsed = start.elapsed().as_secs();
        if elapsed >= next_tick {
            eprintln!("  ...{}s / {}s", elapsed, duration_secs);
            next_tick += 10;
        }
    }

    let elapsed_secs = start.elapsed().as_secs_f64();
    let snapshots: Vec<SourceMetricsSnapshot> = all_metrics.iter().map(|m| m.snapshot()).collect();

    let report = BenchReport {
        duration_secs,
        sources: snapshots
            .iter()
            .map(|s| source_report(s, elapsed_secs))
            .collect(),
    };

    let json = serde_json::to_string_pretty(&report)?;

    match output {
        Some(path) => {
            std::fs::write(&path, &json)?;
            eprintln!("Report written to {}", path.display());
        }
        None => {
            println!("{}", json);
        }
    }

    // Also print a human-readable summary to stderr
    eprintln!();
    eprintln!("=== BENCH SUMMARY ({:.0}s) ===", elapsed_secs);
    for s in &report.sources {
        eprintln!(
            "  {}  shreds/s={:.0}  coverage={}  win={}  lead={} µs  fec-rec={}",
            s.name,
            s.shreds_per_sec,
            s.coverage_pct.map(|p| format!("{:.0}%", p)).unwrap_or("—".into()),
            s.win_rate_pct.map(|p| format!("{:.0}%", p)).unwrap_or("—".into()),
            s.lead_time_mean_us.map(|u| format!("{:+.0}", u)).unwrap_or("—".into()),
            s.fec_recovered_shreds,
        );
    }

    Ok(())
}

fn source_report(s: &SourceMetricsSnapshot, elapsed_secs: f64) -> SourceReport {
    let coverage_pct = if s.coverage_shreds_expected > 0 {
        Some(s.coverage_shreds_seen as f64 / s.coverage_shreds_expected as f64 * 100.0)
    } else {
        None
    };

    let win_rate_pct = {
        let total = s.txs_first + s.txs_duplicate;
        if total > 0 {
            Some(s.txs_first as f64 / total as f64 * 100.0)
        } else {
            None
        }
    };

    let (lead_mean, lead_min, lead_max) = if s.lead_time_count > 0 {
        let mean = s.lead_time_sum_us as f64 / s.lead_time_count as f64;
        let min = if s.lead_time_min_us == i64::MAX { None } else { Some(s.lead_time_min_us) };
        let max = if s.lead_time_max_us == i64::MIN { None } else { Some(s.lead_time_max_us) };
        (Some(mean), min, max)
    } else {
        (None, None, None)
    };

    SourceReport {
        name: s.name.to_string(),
        shreds_received: s.shreds_received,
        shreds_per_sec: s.shreds_received as f64 / elapsed_secs,
        bytes_received_mb: s.bytes_received as f64 / 1_048_576.0,
        shreds_dropped: s.shreds_dropped,
        slots_attempted: s.slots_attempted,
        slots_complete: s.slots_complete,
        slots_partial: s.slots_partial,
        slots_dropped: s.slots_dropped,
        coverage_pct,
        fec_recovered_shreds: s.fec_recovered_shreds,
        txs_decoded: s.txs_decoded,
        txs_per_sec: s.txs_decoded as f64 / elapsed_secs,
        win_rate_pct,
        lead_time_mean_us: lead_mean,
        lead_time_min_us: lead_min,
        lead_time_max_us: lead_max,
        lead_time_samples: s.lead_time_count,
    }
}
