# Stage 1 Design Note: `lq-sequencer` + Event-Sourced State

**Status:** implemented  
**Scope:** sequencing, WAL, snapshots, replay, state hash — no CLOB matching, no margin, no gateway.

## What and why

Everything that touches money must be a deterministic state machine fed by an
**ordered log**. Stage 1 builds that log:

1. **`LogEntry`** — one command/tick with a **global sequence** (authoritative
   total order) and a **per-market sequence** (book-local order, ST expiry later).
2. **`lq-sequencer`** — the only writer: assigns sequences, appends to a
   CRC-framed WAL **before** applying, then applies to the state machine, then
   may snapshot.
3. **`StateMachine` trait** — `apply(entry)`, `state_hash()`, `encode`/`decode`.
   Same log ⇒ byte-identical hash. No wall-clock, no RNG, no `HashMap` order.
4. **Recovery** — load latest snapshot, replay WAL suffix; hash must match a
   full rebuild from an empty log.

This replaces the role that ad-hoc `EngineState` mutation plays today on the
money path (Stage 2+ move matching/margin onto the log).

## Tradeoffs

| Choice | Why | Cost |
|---|---|---|
| WAL before apply | Crash between append and apply recovers by replay | One extra write on the hot path |
| JSON payload + CRC32 framing | Debuggable, fuzz-friendly, no new binary format | Larger than bincode; fine for Stage 1 |
| SHA-256 state hash | Stable across platforms/Rust versions | Slower than `DefaultHasher` (not stable anyway) |
| `BTreeMap` everywhere in SM | Deterministic iteration for hashing | O(log n) vs HashMap O(1) — correct beats fast here |
| `Decimal::normalize()` in hash | `1.10` and `1.1` must hash equal | Slightly more work per field |
| Logical `ts_ms` on the entry | SM never reads the wall clock | Producers must supply time |
| Single apply thread | Matches locked decision; simple Raft story later | No per-market sharding yet |

## API sketch

```text
Sequencer::append(market, ts_ms, payload)
  → assign global_seq, market_seq
  → wal.append(entry) + fsync policy
  → sm.apply(&entry)
  → maybe snapshot every N entries

rebuild(dir)
  → newest snapshot → sm
  → replay WAL entries with global_seq > snapshot.global_seq
  → verify hash
```

## Entry payloads (Stage 1)

- `PlaceOrder` / `CancelOrder` — order lifecycle commands
- `Fill` — execution result (logged so position updates are event-sourced)
- `MarketTick` — last/mark price observation (oracle lands in Stage 4)

Stage 2 adds matching inside `apply`; Stage 3 adds margin/liquidation entries.

## Verification vs `lq-backtest`

`BacktestRunner` can record a `command_log` (place / cancel-all / fill) while
running. Tests feed that log into the sequencer twice and assert identical
state hashes — the same determinism bar as `deterministic_across_runs`, but
for exchange state rather than PnL metrics.

## Residual risks / missing

- Single-segment WAL (no rotation/compaction yet)
- fsync policy is best-effort per append (configurable later for Raft)
- No cross-replica hash compare (Stage 7)
- Decoder fuzzed with proptest arbitrary bytes, not libFuzzer (nightly)
- `command_log` recording is test-oriented; production emit path is Stage 6
