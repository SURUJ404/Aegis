# Stage 4 Design Note: `lq-oracle` — Multi-Venue Aggregation, Log-Driven Prices, Circuit Breakers

**Status:** implemented  
**Scope:** `lq-oracle` (read path: aggregation + observation book; write path:
`OracleBook` embedded by `PerpsState`), the `OraclePrice` log payload, and the
deviation/staleness circuit breakers that gate new-risk entries in `lq-perps`.
No gateway/auth (Stage 5), no funding-interval daemon (moved to Stage 5 with
the liquidator daemon).

## What and why

Stage 3 marks positions at the last trade/tick — a manipulation-prone,
single-venue price. Stage 4 installs a dYdX `x/prices`-style oracle in two
halves split by the log boundary:

1. **Read path (off-chain, daemon side):** `ObservationBook` ingests the
   normalized `MarketEvent` stream the existing `lq-market-data` adapters
   already produce (OKX/Binance/Bybit → Tick/Trade/Snapshot), keeps the
   newest observation per venue, and `aggregate()` computes a **median with
   freshness filtering and outlier rejection**. Pure function of
   `(observations, config, now)` — input order never matters.
2. **Write path (in-chain, state machine):** the daemon publishes the
   aggregate as an `EntryPayload::OraclePrice(OraclePriceCmd)` log entry.
   `OracleBook` (embedded in `PerpsState`) validates it and, on acceptance,
   becomes the market's **reference price**: `price_of` prefers
   `oracle.price(market)` → `tick_marks` → `last_trade`.

```
lq-market-data adapters ──MarketEvent──▶ ObservationBook.record
                                              │  newest-per-venue
                                              ▼
                                    aggregate(): freshness → median → outlier
                                              │  Fresh { price, sources }
                                              ▼
                          LogEntry::OraclePrice { price, obs_ts, sources, override }
                                              │  sequencer (WAL/snapshot/replay)
                                              ▼
                    PerpsState::apply ──▶ OracleBook.apply_price ──▶ OraclePublished
                                              │ gate() = halted/stale?
                                              ▼
                            place/replace/liquidate/settle-funding refused
```

## OracleBook (write path)

One `OracleEntry` per covered market:

```rust
OracleEntry { price, published_ts_ms, observation_ts_ms, sources, halted }
```

`apply_price(market, cmd, entry_ts_ms)` — validation order, first failure wins,
every failure increments `stats.rejected`:

| # | Check | Rejection reason |
|---|---|---|
| 1 | `price > 0` | `invalid_price` |
| 2 | `observation_ts_ms ≤ entry_ts_ms` (no future observations) | `observation_in_future` |
| 3 | `entry_ts_ms − observation_ts_ms ≤ max_staleness_ms` (`0` = off) | `stale_observation` |
| 4 | `sources ≥ min_sources` | `insufficient_sources` |
| 5 | deviation band vs previous accepted price (unless `override_band`) | `oracle_deviation` |

**Deviation breaker (row 26):** a publication moving more than
`max_deviation_bps` (bps of the previous accepted price) is **rejected — the
price does not move — and latches `halted`** for the market (`stats.halts`
counts latch transitions, not repeated rejections). The halt clears only when
an **in-band** publication or an explicit **`override_band`** publication is
accepted; the accepted price re-baselines the band. Defaults:
`max_deviation_bps = 1000` (10 %), `max_staleness_ms = 30_000`,
`min_sources = 1`; `0` disables the corresponding check (the `lq-risk` /
`lq-perps` convention).

**Staleness gate (row 27), log-driven:** `gate(market, ts_ms)` returns
`Some("oracle_halted")` while latched (halt has priority), else
`Some("oracle_stale")` when `ts_ms − published_ts_ms > max_staleness_ms`,
else `None`. Markets with **no accepted publication are uncovered and pass
through** (legacy tick/trade marks apply) — an oracle outage cannot halt a
market that never had a price. Because both times are entry/log times, the
same log always yields the same verdict: no wall clock inside the SM.

**Gate placement in `PerpsState::apply`** (before margin, after sequencing):

| Entry | Gate behavior |
|---|---|
| `PlaceOrder` / `ReplaceOrder` | refused (`oracle_halted` / `oracle_stale`), `stats.oracle_rejected += 1`, no book mutation |
| `Liquidate` | refused before the health check — a stale/halted mark must not price a bankruptcy close |
| `SettleFunding` | refused — funding at an unvouched mark moves collateral on bad numbers |
| `CancelOrder`, `Transfer`, `Fill`, `MarketTick`, `OraclePrice` | **never gated** (risk-reducing / infrastructure entries) |

Every refusal is an `Ok` path with a single `ApplyOutput::Rejected`
(`Uuid::nil()` for non-order entries); the sequence is still consumed, so the
WAL always replays. Only sequence gaps return `Err` (unchanged).

## New payload

| Payload | Behaviour |
|---|---|
| `EntryPayload::OraclePrice(OraclePriceCmd)` | validate + publish; emits `ApplyOutput::OraclePublished { market, price, sources, ts_ms }` on acceptance, `Rejected` with the table reasons otherwise. Consumes the sequence via a CLOB no-op arm (like `Transfer`/`Liquidate`/`SettleFunding`); the common margin pipeline re-runs afterwards because a price change moves equity (flags/ADL re-evaluation). |

`OraclePriceCmd { price, observation_ts_ms, sources, override_band }` lives in
`lq-sequencer::entry` (payloads and their wire types are the sequencer's; the
field is `override_band` because `override` is a Rust keyword).

## Read path: `ObservationBook` + `aggregate()`

- `extract(event)` maps Tick → last, Trade → trade price, Snapshot → touch
  mid `(bid+ask)/2` (rounded to 8 dp), Delta/Status → `None` (reuse of the
  `lq-market-data` event model — the seam the plan required).
- `record()` keeps the **newest observation per venue** (out-of-order
  updates ignored); keys are `BTreeMap`s.
- `aggregate(obs, cfg, now_ms)`:
  1. drop non-positive prices, future observations, and observations older
     than `max_observation_age_ms` (`0` = off);
  2. reject `InsufficientSources` when fewer than `min_sources` fresh
     observations remain (also when *none* remain, covering
     `min_sources = 0`);
  3. dedupe to newest-per-venue;
  4. median (odd → middle, even → average of middles, rounded to
     `price_scale`);
  5. reject observations farther than `outlier_band_bps` from the median and
     recompute; if fewer than `min_sources` survive (or **zero** — found by
     proptest with `min_sources = 0`), report `ConsensusLost { fresh }`
     instead of taking the median of nothing.
- The result is a **set function**: permutation tests + proptest assert
  input order never changes the outcome.

`AggregateConfig` defaults: `min_sources: 1`, `max_observation_age_ms: 5_000`,
`outlier_band_bps: 100` (1 %), `price_scale: 8`.

## Determinism & hashing

- `OracleBook` is `BTreeMap`-only, `Decimal` money, time only from
  `entry.ts_ms` / `cmd.observation_ts_ms`; no clocks, no RNG, no live reads
  inside the SM (the daemon owns the clock and the feeds).
- **Wire format:** JSON cannot use a struct (`MarketId`) as a map key, so
  `prices` serializes as `Vec<(MarketId, OracleEntry)>` — the same convention
  `PerpsWire` already uses (found by the serde roundtrip test). The
  `PerpsWire.oracle` field is `#[serde(default)]`, so Stage 3 snapshots
  decode to an empty book and hash identically.
- **State hash** (`lq-perps-v1` extended): the oracle contributes its own
  `write_hash` (params, stats, every entry incl. the `halted` latch) plus
  `stats.oracle_rejected`. Same log ⇒ byte-identical hash; a different price,
  counter, or latch ⇒ different hash.

## Verification

- **34 `lq-oracle` tests** — `oracle_aggregation.rs` (16): median odd/even,
  outlier exclusion with quorum kept, stale/future/non-positive filtering,
  duplicate-venue newest-wins, `ConsensusLost`, permutation determinism,
  `extract` for Tick/Trade/Snapshot/Status, record/aggregate; **proptest**:
  `aggregate_never_panics` (seeded regression: `min_sources = 0` with all
  observations filtered) and `fresh_price_stays_within_input_range` (median
  within input range + order independence).
  `oracle_book.rs` (18): validation order (first failure wins), deviation
  reject + latch (`halts` counts transitions), in-band clear, override
  re-baseline (and the new band measured from it), `0` = disabled for each
  param, stale gate boundary (exactly `max_staleness_ms` still fresh), halt
  priority over staleness, uncovered passes, hash reflects price/params/latch,
  serde roundtrip preserves equality + hash.
- **21 `lq-perps` oracle behavior tests** (`oracle_behavior.rs`):
  `OraclePublished` output + reference price + oracle precedence over ticks,
  equity marked at the oracle price, deviation halts and block places (no
  book mutation, margin check never runs), counters, override re-baseline,
  cancel/transfer allowed while halted while liquidate is not, staleness
  blocks place/liquidate/settle-funding until a fresh publish (liquidation
  then falls through to `healthy_subaccount`, proving the gate — not health —
  was the blocker), state-level quorum/observation rejections, rejected
  publication consumes the sequence, seq gap remains the only `Err`, gate
  export for the Gateway, hash divergence, plus the **replay trio + encode /
  decode roundtrip on an oracle-bearing log** and the Stage 3 wire-compat
  test (snapshot without the `oracle` field). The fixture asserts
  `check_invariants` after **every** entry.
- **proptest invariants** (`oracle_invariants.rs`): seeded 64-entry logs
  mixing transfers/ticks/trading/liquidations/funding **and oracle
  publications** (in-band, deviation trips that gate later entries, override,
  stale/future observation rejections, gate refusals of place/liquidate/
  funding) — block invariants after every entry, double-run output/hash
  equality, published-output count == `stats.published`, wire roundtrip.
- **Benchmarks** (criterion, this machine):

  | Bench | mean |
  |---|---|
  | `aggregate_5_venues` | ~456 ns |
  | `oracle_apply_price_accept` | ~55 ns |
  | `oracle_gate` | ~6.9 ns |

- Workspace: **269 tests pass, 0 fail**; `cargo clippy --all-targets` clean
  for `lq-sequencer`, `lq-clob`, `lq-oracle`, `lq-perps`.

## Tradeoffs

| Choice | Why | Cost |
|---|---|---|
| Publication payload in `lq-sequencer`, book in `lq-oracle`, gate in `lq-perps` | keeps payload/wire ownership where every other payload lives; dYdX keeper split (`x/prices` state vs `x/clob` consumption) | perps now depends on `lq-oracle`, which pulls `lq-core` (and thus tokio transitively) into the perps dependency tree |
| Deviation rejection **latches** rather than just refusing the price | a breaker that only refuses still lets operators keep spamming bad prices with no gate on trading; latching ties price publication to risk gating | one bad feed can halt new risk until an in-band/override publish; operator must act |
| Staleness judged against `published_ts_ms` in log time | deterministic replay; no wall clock in the SM | a stalled daemon freezes new risk only once entries keep arriving with growing `ts_ms` — with a completely dead log nothing moves anyway (Gateway/Stage 7 liveness) |
| Uncovered markets pass the gate | an oracle outage must not brick markets that trade on ticks today | first publication has no quorum history — `min_sources` and the observation checks are the only guard until a baseline exists |
| `override_band` as an explicit flag on the command | re-baselining must be auditable in the log, not a side-channel | a compromised signer (Stage 5) could override — signer policy belongs to the gateway stage |
| Read/write split across the log boundary | aggregation needs wall-clock freshness and venue dedup; the SM must not | two configs to keep consistent (`AggregateConfig` daemon-side, `OracleParams` in-state); only `OracleParams` is hashed |

## Residual risks / missing

- **Funding interval & liquidator daemons are Stage 5** (gap row 18 was
  re-pointed): `SettleFunding`/`Liquidate` entries exist and are now
  oracle-gated, but nothing schedules them; the read-path `ObservationBook`
  has no daemon polling venue adapters either — Stage 5 wires feeds →
  aggregate → publish and exposes `oracle_gate` to CheckTx.
- The daemon currently trusts its own aggregation result; on-chain
  cross-validation of `sources` (signed venue quotes, dYdX pricefeed
  signatures) is out of scope until signer policy lands with the Gateway.
- Deviation band and staleness are global `OracleParams` — per-market params
  (dYdX has per-market exchange params) are a future extension; `MarketParams`
  already exists as the per-market home.
- No explicit "oracle halted" event in the output stream — halt transitions
  are observable only via `Rejected { reason: "oracle_deviation" }` +
  `oracle().is_halted()`; an indexer-facing event (Stage 6) should surface it.
- `ObservationBook::on_market_event` maps events to markets at the call site;
  venue-symbol → internal `MarketId` mapping is daemon configuration (Stage 5).
- Liquidations currently stop at the gate; there is still no daemon
  submitting `Liquidate` entries (Stage 5/6).
