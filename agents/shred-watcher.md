# shred-watcher — Reference Notes

Source: /Users/jared/Documents/GitHub/shred-watcher (malbeclabs/shred-watcher, private)

## What it is

A Rust tool that listens to raw Solana shreds over unicast UDP and detects Jupiter DEX swap instructions in real time — before transactions are confirmed on-chain. MEV surveillance via early shred visibility.

**Delivery model**: Unicast only. Expects a Solana validator or turbine relay to forward shreds directly to its UDP port (e.g. `--shred-receiver-address <ip>:8001`). Does NOT join multicast groups.

---

## Architecture

```
UDP socket (single async reader, tokio)
        │
        ├──► broadcast channel
        │         ├──► worker 0 ──► ShredAssembler ──► jupiter::try_decode ──► log
        │         ├──► worker 1 ──► ShredAssembler ──► jupiter::try_decode ──► log
        │         └──► worker N ──► ...
        │
        └── recv loop (single, blocks on socket.recv_from)
```

Four modules:

| File | Role |
|------|------|
| `src/main.rs` | CLI, socket setup, broadcast channel, spawn workers |
| `src/shred.rs` | Parse raw UDP bytes → typed `Shred` structs |
| `src/assembler.rs` | Buffer data shreds per slot; flush to entries when complete |
| `src/jupiter.rs` | Match Anchor discriminators; decode swap args |

---

## UDP Socket Configuration

**File**: `src/main.rs`

```rust
let socket = UdpSocket::bind(&cli.bind).await?;   // default: 0.0.0.0:8001
```

Receive buffer (via raw libc):
```rust
libc::setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &size, size_of::<c_int>())
// default size: 268,435,456 (256 MB)
// requires: root or /proc/sys/net/core/rmem_max >= 268435456
```

Interface binding (optional, `--iface`):
```rust
libc::setsockopt(fd, SOL_SOCKET, SO_BINDTODEVICE, name.as_ptr(), iface.len())
// requires: root or CAP_NET_RAW
```

Packet receive loop (single async reader):
```rust
let mut buf = vec![0u8; 1280];   // standard Solana shred MTU
loop {
    let (len, _peer) = socket.recv_from(&mut buf).await?;
    let _ = tx.send(buf[..len].to_vec());
}
```

**Not used**: `SO_BUSY_POLL`, `SO_REUSEPORT`, `SO_REUSEADDR`, multicast options.

---

## Shred Parsing

**File**: `src/shred.rs`

### Byte offsets (all confirmed from source)

| Offset | Field | Type |
|--------|-------|------|
| 0–63 | Signature | [u8; 64] |
| 64 | Variant | u8 |
| 65–72 | Slot | u64 LE |
| 73–76 | Index | u32 LE |
| 77–78 | Shred version | u16 LE |
| 79–82 | FEC set index | u32 LE |
| 83–84 | Parent offset *(data only)* | u16 LE |
| 85 | Flags *(data only)* | u8 |
| 86–87 | Payload size *(data only)* | u16 LE |
| 88+ | Payload *(data only)* | [u8] |

Minimum packet size: 88 bytes.

### Variant detection

```rust
const LEGACY_DATA: u8 = 0xA5;
const LEGACY_CODE: u8 = 0x5A;
const VARIANT_MASK: u8 = 0xC0;
const MERKLE_DATA_TAG: u8 = 0x80;   // bits 7-6 = 0b10
const MERKLE_CODE_TAG: u8 = 0x40;   // bits 7-6 = 0b01

fn classify_variant(v: u8) -> Result<ShredKind> {
    match v {
        LEGACY_DATA => Ok(ShredKind::Data),
        LEGACY_CODE => Ok(ShredKind::Code),
        v if v & VARIANT_MASK == MERKLE_DATA_TAG => Ok(ShredKind::Data),
        v if v & VARIANT_MASK == MERKLE_CODE_TAG => Ok(ShredKind::Code),
        _ => bail!("unknown ShredVariant: 0x{v:02x}"),
    }
}
```

### Data shred flags (byte 85)

- Bit 7 (`0x80`): `last_in_slot`
- Bit 6 (`0x40`): `data_complete`

### Parsed struct

```rust
pub struct Shred {
    pub slot: u64,
    pub index: u32,
    pub version: u16,
    pub fec_set_index: u32,
    pub kind: ShredKind,     // Data | Code
    pub payload: Vec<u8>,    // bytes 88..88+size
    pub last_in_slot: bool,
    pub data_complete: bool,
    pub parent_offset: u16,
}
```

Code shreds are parsed but **not used for FEC recovery** — they are dropped.

---

## Slot Assembly

**File**: `src/assembler.rs`

### Data structures

```rust
pub struct ShredAssembler {
    buffers:  DashMap<u64, BTreeMap<u32, Vec<u8>>>,  // slot → (index → payload)
    complete: DashMap<u64, bool>,                     // slot → done flag
}
```

`DashMap` = lock-free concurrent hashmap. `BTreeMap` keeps payloads sorted by shred index automatically.

### Push logic

```rust
pub fn push(&mut self, shred: Shred) -> Option<Vec<Entry>> {
    if shred.kind != ShredKind::Data || shred.payload.is_empty() {
        return None;   // drop code shreds and empty data shreds
    }
    self.buffers.entry(shred.slot).or_default()
        .insert(shred.index, shred.payload);

    if shred.data_complete || shred.last_in_slot {
        self.complete.insert(shred.slot, true);
    }

    if self.complete.contains_key(&shred.slot) {
        self.try_assemble(shred.slot)
    } else {
        None
    }
}
```

### Assembly and flush

```rust
fn try_assemble(&mut self, slot: u64) -> Option<Vec<Entry>> {
    let (_, frags) = self.buffers.remove(&slot)?;
    self.complete.remove(&slot);

    // Concatenate payloads in index order
    let blob: Vec<u8> = frags.into_values().flatten().collect();

    // Bincode-deserialize Solana Entry structs
    let entries: Vec<solana_entry::entry::Entry> = bincode::deserialize(&blob).ok()?;
    let txs = entries.into_iter().flat_map(|e| e.transactions).collect();
    Some(vec![Entry { slot, transactions: txs }])
}
```

### Eviction policy

Flush is triggered by `data_complete` (bit 6) or `last_in_slot` (bit 7). After flush, slot is removed from both maps. **No timeout**, no max-slot limit, no memory cap — unbounded in-memory accumulation if slots never complete.

**Risk**: If a completing shred arrives before earlier fragments, the slot is flushed with partial data. No retry.

---

## Jupiter Detection

**File**: `src/jupiter.rs`

### Program IDs

```rust
const JUP_V6: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const JUP_V4: &str = "JUP4Fb2cqiRUcaTHdrPC8h2gNsA2ETXiPDD33WcGuJB";
```

### Anchor discriminators (first 8 bytes of instruction data)

`sha256("global:<fn_name>")[0..8]`:

| Discriminator | Instruction |
|---------------|-------------|
| `e5 17 cb 97 7a e3 ad 2a` | `route` |
| `c1 20 9b 30 75 88 08 8f` | `sharedAccountsRoute` |
| `d0 33 ef 97 7b 2b ed d4` | `exactOutRoute` |
| `b0 d1 69 a8 9a 37 8b 8a` | `sharedAccountsExactOut` |
| `0e ef 71 11 dc 55 19 06` | `routeWithTokenLedger` |
| `45 08 6a f2 f3 f6 3d 6e` | `sharedAccountsRouteWithLedger` |

### Argument parsing

Uses a **fixed-tail** strategy (variable-length `route_plan` precedes fixed fields):

```rust
const TAIL: usize = 8 + 8 + 2 + 1;   // 19 bytes from end
// Layout (tail): in_amount(u64) | quoted_out_amount(u64) | slippage_bps(u16) | platform_fee_bps(u8)
let in_amount         = u64::from_le_bytes(tail[0..8]);
let quoted_out_amount = u64::from_le_bytes(tail[8..16]);
let slippage_bps      = u16::from_le_bytes(tail[16..18]);
let platform_fee_bps  = tail[18];
```

### Detection flow

For each transaction, iterates instructions → checks `program_id_index` against static account keys → if Jupiter V4/V6, matches discriminator → decodes args.

**Key limitation**: Uses `msg.static_account_keys()` only. Transactions using Address Lookup Tables (ALTs) to reference Jupiter are missed.

### Output

```
INFO [slot=312847291] [JUP v6] sig=3xKpT7aQbcNv | JupiterSwap {
    instruction: "sharedAccountsRoute",
    in_amount: Some(5000000000),
    quoted_out_amount: Some(482317),
    slippage_bps: Some(50),
    platform_fee_bps: Some(0),
}
```

---

## CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--bind` | `0.0.0.0:8001` | UDP listen address:port |
| `--recv-buf` | `268435456` (256 MB) | Kernel socket receive buffer |
| `--workers` | `4` | Parallel packet-processing workers |
| `--iface` | *(none)* | Bind to specific NIC via `SO_BINDTODEVICE` (root/`CAP_NET_RAW`) |

`RUST_LOG` env var controls log verbosity (default `shred_watcher=info`).

---

## Dependencies (Cargo.toml)

| Crate | Version | Purpose |
|-------|---------|---------|
| `tokio` | 1 (full) | Async runtime, UDP socket, broadcast channel |
| `anyhow` | 1 | Error handling |
| `libc` | 0.2 | `setsockopt` for `SO_RCVBUF`, `SO_BINDTODEVICE` |
| `tracing` | 0.1 | Structured logging |
| `tracing-subscriber` | 0.3 | `RUST_LOG` env filter |
| `dashmap` | 5 | Lock-free concurrent hashmap (slot buffers) |
| `bincode` | 1 | Deserialize Solana `Entry` structs |
| `bs58` | 0.5 | Signature → readable string |
| `clap` | 4 (derive) | CLI argument parsing |
| `solana-sdk` | 1.18 | `Pubkey`, `VersionedTransaction`, `CompiledInstruction` |
| `solana-entry` | 1.18 | `Entry` struct for bincode deserialization |
| `bytes` | 1 | Byte buffer utilities |

---

## Relationship to shredder and shred-probe

| | shred-watcher | shredder | shred-probe |
|---|---|---|---|
| **Language** | Rust | Go | Rust |
| **Role** | Swap detector | Multicast relay | Latency benchmark |
| **Reception** | Unicast UDP (port 8001) | UDP multicast (DoubleZero) | UDP multicast (DoubleZero) |
| **FEC recovery** | No | No | Yes (Reed-Solomon) |
| **Transaction decoding** | Yes (Jupiter swaps) | No | Yes (all txs via bincode) |
| **Lead-time measurement** | No | No (kernel timestamps available) | Yes (vs RPC baseline) |
| **Publisher tracking** | No | Yes (per-IP per-slot) | No |
| **Consumer delivery** | No (self-contained) | Yes (UDP + gRPC to N consumers) | No (self-contained) |
| **Heartbeat port 5765** | N/A (unicast) | Listens on 5765 | Filters 5765 from sniff |

**Typical wiring**: shredder (multicast receiver) → forwards via UDP to shred-watcher (`--shred-receiver-address <ip>:8001`). shred-probe runs independently on the same multicast groups for latency measurement.

---

## Source files

```
/Users/jared/Documents/GitHub/shred-watcher/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── shred.rs
│   ├── assembler.rs
│   └── jupiter.rs
└── tests/
    ├── shred_tests.rs
    ├── assembler_tests.rs
    └── jupiter_tests.rs
```
