# shredder — Reference Notes

Source: /Users/jared/Documents/GitHub/shredder (malbeclabs/shredder, private)

## What it is

A **high-performance Solana shred relay** written in Go. It receives raw shreds from UDP multicast groups (DoubleZero), deduplicates them with a slot-based window, and fans them out to multiple downstream consumers via UDP or gRPC streaming. It also tracks publisher activity (which validators are sending, per slot) for reward eligibility detection and retransmission analysis.

This is a relay / infrastructure service — it does **not** reconstruct blocks via FEC, does not measure lead time itself, and does not decode transactions. It is designed to run on a DoubleZero-connected node and feed downstream consumers.

---

## Architecture

```
UDP Multicast (DoubleZero groups)
         │
         ▼
┌─────────────────────────┐
│ MulticastReceiver        │  recvmmsg batch (64 pkts/syscall)
│ SO_TIMESTAMPNS + PKTINFO │  kernel timestamps, per-group metadata
└──────────┬──────────────┘
           │
           ▼
┌─────────────────────────┐
│ Processing Pipeline      │
│  1. ParseShredID (fast)  │  0.3 ns, zero-alloc
│     or ParseExtended     │  19 ns (validation enabled)
│  2. Publisher tracking   │  RecordPacket (pre-dedup)
│  3. Deduplication        │  FNV-1a slot+index+type hash, 32-slot window
│  4. Publisher tracking   │  RecordUniqueShred (post-dedup)
│  5. Pool & broadcast     │  ref-counted 1500-byte buffers
└──────────┬──────────────┘
           │
     ┌─────┴──────┐
     ▼            ▼
UDP Transport   gRPC Transport
(direct send)   (streaming, type filter)
     │            │
  Consumers    gRPC Subscribers
```

---

## Reception

**File**: `internal/receiver/multicast.go`, `internal/receiver/socket.go`

- **Protocol**: UDP4 multicast via `golang.org/x/net/ipv4.PacketConn`
- **Batch reads**: `ReadBatch()` — reads up to 64 packets per syscall (configurable)
- **Buffer**: Pre-allocated 1500-byte per-packet buffers; zero-copy hot path
- **Socket options**:
  - `SO_RCVBUF`: 256 MB (configurable)
  - `IP_PKTINFO`: Extracts destination multicast group per packet
  - `IP_MULTICAST_LOOP`: Enabled (for localhost testing)
- **Kernel timestamping**:
  - Linux: `SO_TIMESTAMPNS` → nanosecond kernel receive time
  - macOS: Falls back to `time.Now()` (no-op stub in `timestamp_darwin.go`)
- **Heartbeat listener**: Separate UDP socket on port 5765, same multicast groups; detects publishers even between leader slots

---

## Shred Format

**File**: `internal/shred/shred.go`

### Common header (83 bytes)

| Bytes | Field | Type |
|-------|-------|------|
| 0–63 | Signature | [64]byte (ed25519) |
| 64 | Variant | uint8 |
| 65–72 | Slot | uint64 LE |
| 73–76 | Index | uint32 LE |
| 77–78 | Shred version | uint16 LE |
| 79–82 | FEC set index | uint32 LE |

### Variant byte encoding

| Range | Type |
|-------|------|
| `0xA5` | Legacy Data |
| `0x5A` | Legacy Coding |
| `0x80–0x8F` | Merkle Data |
| `0x90–0x9F` | Merkle Data Chained |
| `0xB0–0xBF` | Merkle Data Chained+Resigned |
| `0x40–0x4F` | Merkle Coding |
| `0x60–0x6F` | Merkle Coding Chained |
| `0x70–0x7F` | Merkle Coding Chained+Resigned |
| ≥ `0xC0` | Invalid (rejected by validation) |

### Data shred trailer (bytes 83+)

| Bytes | Field |
|-------|-------|
| 83–84 | Parent offset (uint16) |
| 85 | Flags (`0x01`=DataComplete, `0x02`=LastInSlot) |
| 86–87 | Payload size (uint16) |
| 88+ | Payload |

### Coding shred trailer (bytes 83+)

| Bytes | Field |
|-------|-------|
| 83–84 | Num data shreds (uint16) |
| 85–86 | Num coding shreds (uint16) |
| 87–88 | FEC position (uint16) |

### Parse modes

- **`ParseShredID()`** — fast path (dedup only): extracts slot+index+type in ~0.3 ns, zero alloc, requires 77 bytes minimum
- **`ParseExtended()`** — full parse: all fields, ~19 ns, 1 alloc, requires 83–89 bytes; used when validation is enabled

---

## Deduplication

**File**: `internal/dedup/deduplicator.go`

- Per-slot `sync.Map` of hashes → `map[uint64]struct{}`
- **Hash**: FNV-1a over slot (8B) + index (4B) + type (1B) → uint64
- **Window**: 32 slots (configurable) ≈ 12.8 seconds at 400ms/slot
- **Eviction**: When new slot > `maxSlot - window`; lock-free CAS on max slot
- **Collision probability**: ~1/2^64 per pair (speed over crypto)
- **Metrics**: `shredder_dedup_entries`, `shredder_dedup_evictions_total`, `shredder_dedup_max_slot`

---

## Validation (Optional)

**File**: `internal/shred/shred.go`

Disabled by default; adds ~2 ns per shred when enabled.

**Tier 1 — Header sanity** (before dedup):
- Variant ≥ `0xC0` → reject
- Shred version mismatch (if configured)
- Slot too old (outside dedup window) or too far ahead (`max_slot_distance`)
- Index ≥ 32,768

**Tier 2 — Structural** (on sufficient bytes):
- FEC set index ≤ shred index
- Data parent offset ≤ slot
- Payload size within bounds
- Coding FEC counts non-zero and sum ≤ 32,768

**Tier 3 — Signature** (not implemented): Would require Ed25519 + leader schedule; ~2–20 µs per shred.

---

## Publisher Tracking

**File**: `internal/publisher/tracker.go`

Tracks which validator IPs are sending, per slot. Used for reward eligibility detection and distinguishing leader publishing from retransmission.

```
publisherState (per source IP, uint32 big-endian)
├── slots: sync.Map[slot → slotActivity]
│   ├── totalPackets  atomic.Uint64  (all arrivals, pre-dedup)
│   ├── uniqueShreds  atomic.Uint64  (first-arrival, post-dedup)
│   ├── firstSeen     atomic.Int64   (unix nanos)
│   └── lastSeen      atomic.Int64
├── lastShred         atomic.Int64   (survives slot eviction)
├── lastHeartbeat     atomic.Int64
└── heartbeatCount    atomic.Uint64
```

- `RecordPacket()` called **before** dedup → captures retransmitters
- `RecordUniqueShred()` called **after** dedup → genuine leader signal
- Retention: 750 slots (~5 min at 2.5 slots/sec), configurable
- Memory: ~300 KB for 1,000 publishers (realistic), ~75 MB worst-case

---

## Heartbeat Protocol

**File**: `internal/receiver/heartbeat.go`

- **Port**: 5765 (DoubleZero well-known heartbeat port)
- **Packet**: 4 bytes — magic `"DZ"` + version `0x0001` (`0x44 0x5A 0x00 0x01`)
- **Purpose**: Detect publisher presence even when not currently leader (no shreds flowing)
- **Same multicast groups** as main receiver
- Recorded in publisher tracker as `lastHeartbeat`
- **shred-probe note**: Port 5765 packets are filtered OUT of our traffic sniff (they are heartbeats, not shred data ports)

---

## Consumer Delivery

**File**: `internal/transport/`, `internal/consumer/manager.go`

### UDP transport
- Per-consumer dialed UDP connections, reused
- Raw shred bytes written directly
- Non-blocking; drops on write error

### gRPC transport
- Server on `0.0.0.0:50051` (configurable)
- `Subscribe` RPC: server-streaming, optional `ShredType` filter
- Shred message: `{data, slot, index, type, received_at_ns}`
- `GetPublishers` RPC: per-IP per-slot activity snapshot
- `GetStats` RPC: service-wide counters

### Shred pooling
- Reference-counted 1500-byte buffers (zero GC pressure on hot path)
- `AddRef()` before broadcast, `Release()` in each consumer after processing

### Per-consumer metrics
- `shredder_forwarded_total{consumer_id, transport, type}`
- `shredder_consumer_drops_total` (slow consumer)
- `shredder_forward_latency_seconds` histogram (kernel receipt → delivery, buckets 1µs–10ms)

---

## Configuration

**File**: `internal/config/config.go`

```yaml
receiver:
  listen_addr: "0.0.0.0:5000"
  multicast_groups: ["233.84.178.1", "233.84.178.2"]  # DoubleZero groups
  interface: "doublezero1"
  buffer_size: 268435456   # 256 MB
  batch_size: 64
  heartbeat:
    enabled: true
    listen_addr: "0.0.0.0:5765"

dedup:
  slot_window: 32           # ~12.8s at 400ms/slot

validation:
  enabled: false
  shred_version: 0          # 0 = any
  max_slot_distance: 1000

transports:
  udp:
    enabled: true
  grpc:
    enabled: true
    listen_addr: "0.0.0.0:50051"

consumers:
  static:
    - id: "my-validator"
      transport: "udp"
      address: "10.0.0.10:8001"   # shred-watcher listen port

publisher:
  retention_slots: 750      # ~5 min

metrics:
  enabled: true
  listen_addr: "0.0.0.0:9090"

log:
  level: "info"
  format: "json"
```

All fields also overridable via `SHREDDER_*` environment variables (e.g., `SHREDDER_RECEIVER_MULTICAST_GROUPS`).

---

## Prometheus Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `shredder_received_total` | `type` | All received shreds |
| `shredder_duplicates_total` | `type` | Duplicates filtered |
| `shredder_forwarded_total` | `consumer_id, transport, type` | Delivered to consumers |
| `shredder_validation_rejected_total` | `reason` | Rejected by validation |
| `shredder_feed_packets_total` | `group` | Per multicast group |
| `shredder_feed_wins_total` | `group` | First-arrival wins per group |
| `shredder_dedup_entries` | — | Current dedup map size |
| `shredder_dedup_max_slot` | — | Highest slot seen |
| `shredder_publishers_active` | — | Unique source IPs tracked |
| `shredder_forward_latency_seconds` | `consumer_id` | Kernel→consumer histogram |
| `shredder_consumer_drops_total` | `consumer_id` | Slow-consumer drops |
| `shredder_heartbeats_received_total` | — | Heartbeat packets |

`shredder_feed_wins_total` per group enables multi-feed comparison: which DoubleZero group delivers each shred first.

---

## Bundled Tools

### shredmon
Real-time monitoring dashboard connecting to a running shredder instance.

```bash
./shredmon                                    # TUI via gRPC (localhost:50051)
./shredmon -transport udp -addr 0.0.0.0:6000  # UDP direct
./shredmon -no-tui -verbose                   # JSON lines
```

Displays: current slot + age, shreds/sec (data/coding), FEC set stats, parent offset, version, windowed + cumulative counters.

### shredctl
CLI for inspecting shredder state via gRPC.

```bash
./shredctl publishers   # per-IP per-slot activity
./shredctl stats        # service-wide counters
./shredctl consumers    # consumer status
./shredctl --json       # JSON output
```

### Other tools
- **shredgen**: Generates synthetic shreds for load testing
- **grpcclient**: Raw gRPC client for testing Subscribe/GetPublishers RPCs
- **udprecv**: Standalone UDP listener for debug

---

## Performance

| Operation | Cost |
|-----------|------|
| `ParseShredID` (dedup path) | 0.3 ns |
| `ParseExtended` (validation path) | 19 ns |
| FNV-1a hash | < 1 ns |
| Dedup lookup | ~100 ns |
| Publisher tracking (atomic) | 17 ns |
| UDP send to consumer | ~10 µs |
| gRPC channel enqueue | < 1 µs |

**Throughput**: 30,000+ shreds/sec on a single core (exceeds typical Solana mainnet rate).

**Memory** (30k shreds/sec, 1,000 publishers):
- Dedup window (32 slots): 10–20 MB
- Publisher tracker (750-slot retention): ~300 KB
- Packet pool (64 batch × 1500B): 96 KB
- **Total**: ~20–40 MB heap

---

## Relationship to shred-probe and shred-watcher

| | shredder | shred-probe | shred-watcher |
|---|---|---|---|
| **Language** | Go | Rust | Rust |
| **Role** | Relay / infrastructure | Latency benchmark | Swap detector |
| **Reception** | UDP multicast (DoubleZero) | UDP multicast (DoubleZero) | Unicast UDP (port 8001) |
| **FEC recovery** | No | Yes (Reed-Solomon) | No |
| **Deduplication** | Yes (slot window) | Yes (signature-based) | No |
| **Lead-time measurement** | No (kernel timestamps available) | Yes (vs RPC baseline) | No |
| **Publisher tracking** | Yes (per-IP per-slot) | No | No |
| **Consumer delivery** | UDP + gRPC to N consumers | Internal only | Internal only |
| **Transaction decoding** | No | Yes (bincode Entry) | Yes (Jupiter swaps) |
| **Heartbeat port** | Listens on 5765 | Filters out 5765 | N/A |
| **Metrics** | Prometheus + gRPC | JSONL log | None |

**Typical deployment**: shredder runs as the multicast receiver and relay; shred-watcher connects to shredder as a UDP consumer (address `validator:8001`); shred-probe runs independently on the same multicast groups to measure latency.

---

## Health Endpoints

- `GET /healthz` — liveness (200 if running)
- `GET /readyz` — readiness (200 if all components started)
- `GET /metrics` — Prometheus metrics (port 9090)

---

## Build & Run

```bash
make build          # produces ./bin/shredder
./bin/shredder --config config.yaml
go test -race ./... # unit tests
```

E2E tests run in Docker with synthetic 5,000 shreds/sec + 30% duplicates.
