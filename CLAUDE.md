# Agent Instructions — shred-probe

## What this is
Standalone Solana shred feed latency benchmark. Measures the millisecond advantage of raw shred feeds (DoubleZero, Jito ShredStream) over confirmed-block RPC polling.

## Agent context — read these files at session start
| File | Contents |
|---|---|
| `agents/` | TBD |

## Critical rules

### Git identity
**All commits must be authored as `Haruko-Haruhara-GSPB / jwarn2011@gmail.com`.**
Always verify before committing:
```bash
git config user.name   # must be Haruko-Haruhara-GSPB
git config user.email  # must be jwarn2011@gmail.com
```

### Commit style
- Push directly to main — no PRs
- No `Co-Authored-By` trailers in commit messages
- No auto-commits without explicit instruction

### Build
- `cargo build` requires Linux (x86_64)
- `cargo check` works locally on macOS for syntax validation
