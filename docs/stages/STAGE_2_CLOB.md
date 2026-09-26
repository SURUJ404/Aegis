# Stage 2 Design Note: `lq-clob` — Order-Level CLOB

**Status:** implemented  
**Scope:** price-time matching inside `apply`, TIF, STP, cancel/replace, ST vs
stateful orders — no margin/liquidation (Stage 3), no signed gateway (Stage 5).

## What and why

Stage 1 gave the log; Stage 2 puts **matching on it**. `lq-clob`'s `ClobState`
implements `lq_sequencer::StateMachine` so that `apply(LogEntry)` is the single
deterministic transition:

1. **Order-level book** (§4.1 gap): per market, `BTreeMap<Price, VecDeque<Uuid>>`
   per side — best price by map order, FIFO within level. The feed-level
   `lq-orderbook` stays market-data only.
2. **Matching inside `apply`**: no separate fill events, no RNG, no wall clock.
   Taker walks the opposite side; trade price = **resting (maker) price**.
3. **Outputs**: `apply` returns `Vec<ApplyOutput>` (fills with fees, placed /
   cancelled / rejected / expired) for tests now and the indexer (Stage 6).
4. **Replay-safe rejections**: every command-level failure (duplicate id,
   PostOnly cross, FOK unfilled, unknown cancel, self-trade, bad qty/price) is
   an `Ok` path that records `ApplyOutput::Rejected` — the WAL always replays
   through `Sequencer::open`. Only sequence gaps return `Err`.

## Policy matrix

| Input | Behaviour |
|---|---|
| GTC limit | match, rest remainder |
| IOC / market | match, cancel remainder (`CancelReason::IocRemainder`) |
| FOK | pre-check on a cloned book; all-or-nothing (`fook_unfilled` if short) |
| Post-only | reject if it would touch the opposite side (`post_only_cross`) |
| Market | never rests; sweeps until book empty or qty exhausted |

`exec_policy(order_type, tif)`: explicit order types (Market/PostOnly/IOC/FOK)
win over the TIF field.

## Self-trade prevention (`StpPolicy` on the command)

Compares non-empty `owner` strings at the moment a taker would hit a resting
maker, mid-loop:

| Policy | Effect |
|---|---|
| `None` (default) | self-match allowed |
| `CancelResting` | cancel maker, taker continues past it |
| `CancelTaker` | abort taker (no fills → `Rejected`; partial fills → `Cancelled`) |
| `CancelBoth` | cancel maker, then abort taker |

## Cancel/replace & expiry

- `ReplaceOrder { old, new }` — one entry: cancel old, place new. If old is
  gone/terminal the whole command is rejected with **no mutation**.
- **ST vs stateful**: `expiration_ms: Option<u64>` (logical `ts_ms` domain).
  Every apply sweeps *all* markets for `ts_ms >= expiration` → `Expired`.
  `None` = stateful (persists until cancel/fill). Arriving already-expired
  orders are rejected (`already_expired`).

## Determinism & hashing

- `BTreeMap` only; `Decimal` money (fees are Decimal bps in `ClobConfig`,
  part of the hash — replicas must agree on fee config).
- Time exclusively from `entry.ts_ms`.
- State hash (`lq-clob-v1`): config, sequences, every order field, book price
  levels + FIFO queues, positions, fees, marks, stats.

## Verification

- **28 behavioral tests** (`clob_behavior.rs`): price-time priority, better
  price beats earlier time, all TIF branches, FOK atomicity, PostOnly,
  market sweep, three STP modes + none, cancel/replace, ST expiry, validation,
  seq-gap-is-the-only-error, double-run hash equality, empty-log rebuild,
  snapshot midway, encode/decode roundtrip, fee/position accounting.
- **proptest invariants** (`clob_invariants.rs`): random 1–60-op logs on two
  markets — book↔order consistency, no terminal/zero-remaining resting orders,
  never-crossed book, filled ≤ qty, per-order fill totals from outputs equal
  `filled_quantity`, live hash = sequencer/WAL/rebuild hash, outputs equal
  across two independent runs.
- **Benchmarks** (criterion, quick run on this machine):

  | Bench | mean |
  |---|---|
  | `rest_1000_orders` | ~3.04 ms (~3 µs/order) |
  | `sweep_100_asks_market` | ~70 µs (~0.7 µs/fill) |
  | `single_passive_fill` | ~1.6 µs |
  | `state_hash_2000_orders` | ~1.0 ms |

## Tradeoffs

| Choice | Why | Cost |
|---|---|---|
| FOK pre-check clones book+orders | exact multi-level/STP simulation, zero-risk atomicity | O(book) alloc per FOK |
| Rejections are outputs, not `Err` | WAL always replays; seq gaps remain the only `Err` | callers must check outputs |
| Fees in state (`ClobConfig`) | hash covers everything affecting future fills | fee changes need a migration story |
| Single account view of positions | subaccounts land in Stage 3 | STP uses `owner` strings until then |
| Expiry sweep scans all markets per entry | global logical time, simple | O(orders) per apply (fine now) |

## Residual risks / missing

- No margin check on place (Stage 3 adds `lq-perps` pre-trade checks).
- STP `owner` is a plain string — no subaccount id yet.
- `simulate` clones for FOK; measure under heavy FOK load before Stage 7.
- Queue position/partial-queue realism: makers at a level are strict FIFO,
  no queue-jump modeling.
- Legacy `Fill` entries still apply (Stage-1 compat) but emit no outputs.
- Live engine still runs `EngineState`; CLOB not yet wired into the engine
  loop (gateway/ingress is Stage 5; backtest re-base comes later).
