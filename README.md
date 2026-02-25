# shredder

Measures the latency advantage of raw Solana shred feeds over confirmed-block RPC.

If your business depends on seeing transactions before your competitors, shredder gives you an estimate of how many milliseconds ahead you are, and whether that edge is holding.

```
==========================================================================================
                SHREDDER FEED QUALITY DASHBOARD  2026-02-25 14:32:05
==========================================================================================

SOURCE                 SHREDS/s   COV%   WIN%  TXS/s  FEC-REC  LEAD µs (mean / min / max)
------------------------------------------------------------------------------------------
bebop                      4200    82%    61%    400        52  +321 / +180 / +640
jito-shredstream           4100    78%    40%    380        38  +271 / +150 / +590
rpc                           0     —      —     420         0  —
------------------------------------------------------------------------------------------

EDGE:
  ✓ bebop  AHEAD of RPC by avg 0.32ms  (n=12400)
  ✓ jito-shredstream  AHEAD of RPC by avg 0.27ms  (n=9800)
```

---

## How it works

Solana leaders distribute blocks as shreds over UDP. Feed providers relay those shreds to your machine before the block is confirmed. 

shredder:

1. Binds a UDP socket on your multicast interface and receives raw shreds
2. Parses the Agave wire format, runs Reed-Solomon FEC recovery on partial FEC sets
3. Deserializes `Entry` structs via bincode to extract transactions
4. Polls your local RPC node for confirmed blocks in parallel
5. Matches transactions across sources by `signatures[0]`, computes arrival time deltas

Lead time = `T_rpc_confirmed − T_shred_received`. Positive means you were ahead.

All timestamps use `CLOCK_MONOTONIC_RAW` (Linux) — immune to NTP slew.

---

## Requirements

- Linux x86_64
- A shred feed
- A local Solana RPC node (for the baseline comparison)
- Rust 1.81+ _(build from source only)_

---

## Install

**Pre-built binary (recommended):**

```bash
curl -fsSL https://github.com/Haruko-Haruhara-GSPB/shred-probe/releases/latest/download/shredder -o /usr/local/bin/shredder && chmod +x /usr/local/bin/shredder
```

**Build from source (requires Rust 1.81+):**

```bash
git clone https://github.com/Haruko-Haruhara-GSPB/shred-probe.git ~/shred-probe
cargo install --path ~/shred-probe
```

---



## Quick start

Detect active feeds and write `probe.toml`:

```bash
shredder discover
```

Install and start the background service (persists across sessions):

```bash
shredder service install
shredder service start
shredder service enable
```

Check current metrics (non-interactive, works from any terminal):

```bash
shredder status
```

Live dashboard (interactive, Ctrl-C to exit):

```bash
shredder monitor
```

Timed benchmark, JSON output:

```bash
shredder bench --duration 300 --output report.json
```

---

## Configuration

`probe.toml` defines one or more sources. Mix shred feeds and an RPC baseline:

```toml
# DoubleZero bebop feed
[[sources]]
name = "bebop"
type = "shred"
multicast_addr = "233.84.178.1"
port = 20001
interface = "doublezero1"

# Jito ShredStream feed
[[sources]]
name = "jito-shredstream"
type = "shred"
multicast_addr = "233.84.178.2"
port = 20001
interface = "doublezero1"

# RPC baseline
[[sources]]
name = "rpc"
type = "rpc"
url = "http://127.0.0.1:8899"
```

Optional per-source fields:

| Field | Default | Description |
|-------|---------|-------------|
| `port` | `20001` | UDP multicast port |
| `interface` | `doublezero1` | Network interface for multicast |
| `pin_recv_core` | — | CPU core to pin the receiver thread |
| `pin_decode_core` | — | CPU core to pin the decoder thread |

---

## Commands

### `shredder discover`

Diagnostic snapshot before you start. Shows DoubleZero group availability, active multicast memberships on the machine, UDP sockets bound to multicast addresses, and configured sources from `probe.toml`.

### `shredder monitor [--interval N]`

Live-updating dashboard. Refreshes every `N` seconds (default 5). Columns:

| Column | Meaning |
|--------|---------|
| `SHREDS/s` | Raw UDP packets received per second |
| `COV%` | Fraction of each block's data shreds that arrived |
| `WIN%` | Fraction of transactions this source decoded first |
| `TXS/s` | Decoded transactions per second |
| `FEC-REC` | Shreds reconstructed via Reed-Solomon in this window |
| `LEAD µs` | Mean / min / max arrival advantage over RPC (µs) |

Press Ctrl-C to stop.

### `shredder bench --duration N [--output FILE]`

Runs for `N` seconds, then writes a JSON report. If `--output` is omitted, prints to stdout. Human-readable summary goes to stderr.

```json
{
  "duration_secs": 300,
  "sources": [
    {
      "name": "bebop",
      "shreds_received": 1260000,
      "shreds_per_sec": 4200.0,
      "bytes_received_mb": 1488.4,
      "coverage_pct": 82.3,
      "fec_recovered_shreds": 15600,
      "txs_decoded": 126000,
      "txs_per_sec": 420.0,
      "win_rate_pct": 61.4,
      "lead_time_mean_us": 321.4,
      "lead_time_min_us": 95,
      "lead_time_max_us": 980,
      "lead_time_samples": 74800
    }
  ]
}
```

### `shredder init`

Prints a default `probe.toml` to stdout.

---

## Understanding the numbers

**Coverage %** — DoubleZero relays only the tail FEC sets of each block, not the full block. 80–90% coverage is normal and expected. shredder handles mid-stream joins correctly (no waiting for shred index 0).

**Win rate %** — how often this source delivers a transaction before all other sources. With two shred feeds and one RPC, a healthy setup shows the faster shred source winning 55–65% of transactions.

**Lead time** — samples outside `[−500ms, +2000ms]` are discarded as measurement artifacts (e.g. RPC retry delays). The displayed mean/min/max reflect real network latency only.

**FEC recovery** — when data shreds are dropped in transit, Reed-Solomon coding shreds allow reconstruction. A non-zero FEC-REC count is normal; a high count relative to SHREDS/s may indicate packet loss on the multicast path.

---

## DoubleZero multicast groups

| Code | Multicast IP | Description |
|------|-------------|-------------|
| `bebop` | `111.11.111.1` | Malbec Labs relay |
| `jito-shredstream` | `111.11.111.2` | Jito relay |

To subscribe to a multicast group over DoubleZero refer to the [DoubleZero documentation](https://docs.malbeclabs.com/Multicast%20Connection/).

---
## Upgrade

```bash
shredder upgrade
```

---

## Uninstall

If installed via `curl`:
```bash
rm /usr/local/bin/shredder
```

If installed via `cargo install`:
```bash
cargo uninstall shredder
```

Remove config and source:
```bash
rm -rf ~/shred-probe probe.toml
```

---
## License

MIT
