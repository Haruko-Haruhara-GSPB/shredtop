use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// Atomic per-source quality counters.
/// All writes use Relaxed ordering — these are sampling metrics, not synchronisation.
pub struct SourceMetrics {
    pub name: &'static str,

    // Ingestion
    pub shreds_received: AtomicU64,
    pub bytes_received: AtomicU64,
    /// Shreds silently dropped because the receiver→decoder channel was full
    /// (backpressure from the decoder falling behind).
    pub shreds_dropped: AtomicU64,

    // Slot outcomes
    pub slots_attempted: AtomicU64,
    pub slots_complete: AtomicU64,
    pub slots_partial: AtomicU64,
    pub slots_dropped: AtomicU64,

    // Coverage (data shreds)
    pub coverage_shreds_seen: AtomicU64,
    pub coverage_shreds_expected: AtomicU64,

    // FEC recovery
    pub fec_recovered_shreds: AtomicU64,

    // Tx flow
    pub txs_decoded: AtomicU64,
    pub txs_emitted: AtomicU64,
    /// Won the fan-in dedup race (first arrival)
    pub txs_first: AtomicU64,
    /// Lost the fan-in dedup race (duplicate)
    pub txs_duplicate: AtomicU64,

    // Lead time relative to RPC (µs, positive = shred arrived before RPC)
    pub lead_time_count: AtomicU64,
    /// Number of lead-time samples where this source beat RPC (lead_time > 0)
    pub lead_wins: AtomicU64,
    pub lead_time_sum_us: AtomicI64,
    /// Initialised to i64::MAX; updated via CAS loop
    pub lead_time_min_us: AtomicI64,
    /// Initialised to i64::MIN; updated via CAS loop
    pub lead_time_max_us: AtomicI64,
}

/// Plain-struct snapshot of SourceMetrics for display (no atomics).
#[derive(Debug, Clone)]
pub struct SourceMetricsSnapshot {
    pub name: &'static str,
    pub shreds_received: u64,
    pub bytes_received: u64,
    pub shreds_dropped: u64,
    pub slots_attempted: u64,
    pub slots_complete: u64,
    pub slots_partial: u64,
    pub slots_dropped: u64,
    pub coverage_shreds_seen: u64,
    pub coverage_shreds_expected: u64,
    pub fec_recovered_shreds: u64,
    pub txs_decoded: u64,
    pub txs_emitted: u64,
    pub txs_first: u64,
    pub txs_duplicate: u64,
    pub lead_time_count: u64,
    pub lead_wins: u64,
    pub lead_time_sum_us: i64,
    pub lead_time_min_us: i64,
    pub lead_time_max_us: i64,
}

impl SourceMetrics {
    pub fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            shreds_received: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            shreds_dropped: AtomicU64::new(0),
            slots_attempted: AtomicU64::new(0),
            slots_complete: AtomicU64::new(0),
            slots_partial: AtomicU64::new(0),
            slots_dropped: AtomicU64::new(0),
            coverage_shreds_seen: AtomicU64::new(0),
            coverage_shreds_expected: AtomicU64::new(0),
            fec_recovered_shreds: AtomicU64::new(0),
            txs_decoded: AtomicU64::new(0),
            txs_emitted: AtomicU64::new(0),
            txs_first: AtomicU64::new(0),
            txs_duplicate: AtomicU64::new(0),
            lead_time_count: AtomicU64::new(0),
            lead_wins: AtomicU64::new(0),
            lead_time_sum_us: AtomicI64::new(0),
            lead_time_min_us: AtomicI64::new(i64::MAX),
            lead_time_max_us: AtomicI64::new(i64::MIN),
        })
    }

    /// Outlier bounds for lead-time samples (µs).
    /// Samples outside this range are silently discarded — they indicate measurement
    /// artifacts (e.g. RPC block-fetch retry) rather than real network latency.
    pub const LEAD_TIME_MAX_US: i64 = 2_000_000; // 2 000 ms
    pub const LEAD_TIME_MIN_US: i64 = -500_000; //  -500 ms

    /// Record a single lead-time sample in microseconds.
    /// Positive values mean this source arrived before its counterpart.
    /// Samples outside [LEAD_TIME_MIN_US, LEAD_TIME_MAX_US] are discarded.
    pub fn record_lead_time_us(&self, us: i64) {
        if us > Self::LEAD_TIME_MAX_US || us < Self::LEAD_TIME_MIN_US {
            return;
        }
        self.lead_time_count.fetch_add(1, Relaxed);
        if us > 0 {
            self.lead_wins.fetch_add(1, Relaxed);
        }
        self.lead_time_sum_us.fetch_add(us, Relaxed);

        let mut cur = self.lead_time_min_us.load(Relaxed);
        while us < cur {
            match self.lead_time_min_us.compare_exchange_weak(cur, us, Relaxed, Relaxed) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }

        let mut cur = self.lead_time_max_us.load(Relaxed);
        while us > cur {
            match self.lead_time_max_us.compare_exchange_weak(cur, us, Relaxed, Relaxed) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Mean lead time in µs, or None if no samples yet.
    pub fn mean_lead_time_us(&self) -> Option<f64> {
        let count = self.lead_time_count.load(Relaxed);
        if count == 0 {
            return None;
        }
        Some(self.lead_time_sum_us.load(Relaxed) as f64 / count as f64)
    }

    /// Shred coverage as a percentage, or None if no expected count recorded.
    pub fn coverage_pct(&self) -> Option<f64> {
        let expected = self.coverage_shreds_expected.load(Relaxed);
        if expected == 0 {
            return None;
        }
        Some(self.coverage_shreds_seen.load(Relaxed) as f64 / expected as f64 * 100.0)
    }

    /// Fraction of decoded txs that won the fan-in race, or None if no data.
    pub fn win_rate(&self) -> Option<f64> {
        let first = self.txs_first.load(Relaxed);
        let dup = self.txs_duplicate.load(Relaxed);
        if first + dup == 0 {
            return None;
        }
        Some(first as f64 / (first + dup) as f64 * 100.0)
    }

    /// Capture a consistent point-in-time snapshot (no locking; slight skew possible).
    pub fn snapshot(&self) -> SourceMetricsSnapshot {
        SourceMetricsSnapshot {
            name: self.name,
            shreds_received: self.shreds_received.load(Relaxed),
            bytes_received: self.bytes_received.load(Relaxed),
            shreds_dropped: self.shreds_dropped.load(Relaxed),
            slots_attempted: self.slots_attempted.load(Relaxed),
            slots_complete: self.slots_complete.load(Relaxed),
            slots_partial: self.slots_partial.load(Relaxed),
            slots_dropped: self.slots_dropped.load(Relaxed),
            coverage_shreds_seen: self.coverage_shreds_seen.load(Relaxed),
            coverage_shreds_expected: self.coverage_shreds_expected.load(Relaxed),
            fec_recovered_shreds: self.fec_recovered_shreds.load(Relaxed),
            txs_decoded: self.txs_decoded.load(Relaxed),
            txs_emitted: self.txs_emitted.load(Relaxed),
            txs_first: self.txs_first.load(Relaxed),
            txs_duplicate: self.txs_duplicate.load(Relaxed),
            lead_time_count: self.lead_time_count.load(Relaxed),
            lead_wins: self.lead_wins.load(Relaxed),
            lead_time_sum_us: self.lead_time_sum_us.load(Relaxed),
            lead_time_min_us: self.lead_time_min_us.load(Relaxed),
            lead_time_max_us: self.lead_time_max_us.load(Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lead_time_min_max() {
        let m = SourceMetrics::new("test");
        m.record_lead_time_us(1000);
        m.record_lead_time_us(500);
        m.record_lead_time_us(5000);
        m.record_lead_time_us(-200);
        assert_eq!(m.lead_time_count.load(Relaxed), 4);
        assert_eq!(m.lead_time_min_us.load(Relaxed), -200);
        assert_eq!(m.lead_time_max_us.load(Relaxed), 5000);
        let mean = m.mean_lead_time_us().unwrap();
        assert!((mean - 1575.0).abs() < 0.1);
    }

    #[test]
    fn test_lead_time_outlier_cap() {
        let m = SourceMetrics::new("test");
        m.record_lead_time_us(1_000_000);
        m.record_lead_time_us(-400_000);
        m.record_lead_time_us(2_000_001);
        m.record_lead_time_us(6_724_000);
        m.record_lead_time_us(4_994_000);
        m.record_lead_time_us(-500_001);
        assert_eq!(m.lead_time_count.load(Relaxed), 2);
        assert_eq!(m.lead_time_min_us.load(Relaxed), -400_000);
        assert_eq!(m.lead_time_max_us.load(Relaxed), 1_000_000);
    }

    #[test]
    fn test_win_rate() {
        let m = SourceMetrics::new("test");
        assert!(m.win_rate().is_none());
        m.txs_first.fetch_add(7, Relaxed);
        m.txs_duplicate.fetch_add(3, Relaxed);
        let wr = m.win_rate().unwrap();
        assert!((wr - 70.0).abs() < 0.01);
    }

    #[test]
    fn test_coverage_pct() {
        let m = SourceMetrics::new("test");
        assert!(m.coverage_pct().is_none());
        m.coverage_shreds_seen.store(67, Relaxed);
        m.coverage_shreds_expected.store(100, Relaxed);
        let cov = m.coverage_pct().unwrap();
        assert!((cov - 67.0).abs() < 0.01);
    }

    #[test]
    fn test_snapshot() {
        let m = SourceMetrics::new("snap");
        m.shreds_received.store(100, Relaxed);
        m.txs_decoded.store(42, Relaxed);
        let s = m.snapshot();
        assert_eq!(s.name, "snap");
        assert_eq!(s.shreds_received, 100);
        assert_eq!(s.txs_decoded, 42);
    }
}
