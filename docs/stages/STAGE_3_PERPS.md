# Stage 3 Design Note: `lq-perps` — Subaccounts, Margin, Liquidation

**Status:** implemented  
**Scope:** subaccounts + collateral, initial/maintenance margin, pre-trade
checks absorbing `lq-risk` limits, liquidation at the bankruptcy price,
insurance fund, ADL, funding — one atomic `apply` composed over `lq-clob`.
No oracle (Stage 4), no signed gateway (Stage 5).

## What and why

Stage 2 matched orders; Stage 3 makes matching **safe**. `lq-perps`'s
`PerpsState` embeds `ClobState` and is the machine the `Sequencer` drives:
one log entry ⇒ pre-trade margin check → matching → cash/position/margin
update, all inside a single deterministic transition.

1. **Subaccounts** (gap rows 15/22): `Subaccount { collateral, positions }`,
   id `u64`, default `0`, insurance fund reserved at `u64::MAX`. Equity is
   cash-basis: `equity = collateral + Σ qty · price` per market. A fill moves
   cash between the two subaccounts — the fill and its margin transition are
   one `apply`, never two events.
2. **Margin** (row 16): per-market `MarketParams { initial_margin_ratio,
   maintenance_margin_ratio, liquidation_fee_bps }` (defaults 10 % / 5 %).
   Requirement = `|qty| · price · ratio`, summed over positions plus open-order
   reservations for initial margin.
3. **Pre-trade checks** (row 23): `RiskEngine`'s limits live inside `apply` as
   `pre_trade_check` — validation, limits, margin — evaluated **before** the
   CLOB sees the order, and exported for the Stage 5 gateway's CheckTx-style
   validation. The legacy `RiskEngine` remains only on the old engine path.
4. **Liquidation** (row 19): below maintenance ⇒ pending set; `Liquidate`
   executes at the **bankruptcy limit price** against the book, then any
   residual closes against the insurance fund. Cascade breaker caps notional
   per rolling window.
5. **Insurance + ADL** (rows 20/21): fees credit the insurance ledger; if a
   residual close drives its equity negative, ADL closes the largest opposing
   positions at a price that restores insurance equity to zero.
6. **Funding** (row 18): `SettleFunding { rate }` pays `qty · rate · mark`
   across subaccounts — zero-sum by construction — and advances the funding
   index used to keep position bookkeeping consistent.
7. **Block invariants** (row 24): `check_invariants()` — collateral conserved,
   `Σ positions = 0` per market, pending set = recomputed below-maintenance set
   — runs after every apply in tests (an `EndBlocker` analogue for Stage 7).

## Entry pipeline

```
expect_global / expect_market        ← seq gaps are the only Err
pre_trade_check (Place/Replace)      ← Allow / Reduce / Reject + stats
clob.apply / apply_noop / force_cancel   ← exactly one CLOB step per entry
process_clob_outputs                 ← fills ⇒ cash+positions (atomic)
sweep_flat_negatives                 ← zero-balance, empty-position cleanup
run_adl                              ← insurance equity < 0 ⇒ close vs ladder
revalidate_reduce_only               ← RO orders whose position shrank
rebuild_flags                        ← pending liquidations + MarginFlagged
```

Replay-safe: every command failure is `Ok` + `ApplyOutput::Rejected` with a
stable `reason`; only sequence gaps return `Err` (WAL always replays).

## New payloads & command fields

| Payload | Behaviour |
|---|---|
| `Transfer { subaccount, amount }` | deposit (+) / withdraw (−); amount quantized to `PRICE_SCALE` first; rejects `invalid_amount`, `insufficient_collateral`, `reserved_subaccount` |
| `Liquidate { subaccount, max_qty? }` | two-phase close at the bankruptcy limit; rejects `no_position`, `healthy_subaccount`, `liquidation_cascade`, `liquidation_id_collision` |
| `SettleFunding { rate }` | zero-sum payments + funding index; rejects `no_mark_price` |

`PlaceOrderCmd`/`ReplaceOrderCmd` gained `#[serde(default)] subaccount:
Option<u64>` and `reduce_only: bool`. When `subaccount` is `Some`, the STP
owner is normalized to `sub:<n>` so self-trade prevention is per-subaccount.

## Pre-trade matrix (`pre_trade_check`)

| Code | Trigger | Verdict |
|---|---|---|
| `invalid_quantity` / `invalid_price` | non-positive, or scale > `PRICE_SCALE` | Reject |
| `no_mark_price` | no tick/trade reference for the market | Reject |
| `max_order_qty` | `PerpsConfig::max_order_qty` exceeded | **Reduce** (place at capped qty; `lq-risk` `place_checked` parity; not counted in `margin_rejected`) |
| `max_position` / `max_notional` / `max_open_orders` | operator limits (`0` = disabled) | Reject |
| `reduce_only_exceeds_position` | RO order would grow exposure | Reject |
| `insufficient_margin` | initial margin (incl. open-order reservation) not met after the projected fill | Reject |
| `below_maintenance` | projected fill lands below maintenance immediately | Reject |
| `reserved_subaccount` | order targets the insurance ledger | Reject |

A `Reduce` is still a pre-trade pass: the order is placed at the capped
quantity (callers that want a hard reject can compare `qty` themselves).

## Liquidation phases

1. **Flag** — every apply rebuilds the pending set: non-insurance subaccounts
   holding a position with `equity < maintenance` (newly flagged emit
   `MarginFlagged`).
2. **Limit price** — `P = mark + (fee − equity) / close_qty`, with
   `fee = |close_qty| · mark · (taker_bps + liquidation_fee_bps) / 10 000`,
   quantized to `PRICE_SCALE` and clamped to `MIN_PRICE = 1e-8`. For a long
   this is a floor below the mark; for a short, a ceiling above it.
3. **Book phase** — a synthetic IOC (id `0x6C71_6C69_7100_0000 << 64 |
   global_seq`, owner `sub:<n>`) closes up to `max_qty` against the book at
   prices **strictly better than** `P`.
4. **Insurance phase** — the residual closes against the insurance ledger at
   exactly `P`. If that leaves insurance equity negative → ADL.
5. **Cascade breaker** — liquidation notional in the rolling
   `liquidation_window_ms` window may not exceed
   `max_liquidation_notional_per_window` (`liquidation_cascade`).

## ADL

While insurance equity < 0: pick the counterparty with the largest opposite
position (`|qty|` desc, id asc), close it at the price that restores insurance
equity to exactly 0 (quantized to `PRICE_SCALE`), emit `Adl`. Each iteration
removes a position, so the loop terminates; flat markets are never touched.

## Determinism & hashing

- `BTreeMap`/`BTreeSet` only, `Decimal` fixed-point money, time exclusively
  from `entry.ts_ms`, no RNG, no floats in state.
- **`PRICE_SCALE = 9`**: bankruptcy/ADL/funding prices divide by quantities and
  could emit 28-digit decimals; moving those between balances of different
  magnitudes rounds each balance differently and was observed breaking
  collateral conservation by 1e-26 (found by proptest). Every machine-computed
  price/amount (liquidation limit, ADL price, funding delta, transfer amount)
  is quantized to `PRICE_SCALE` first, which keeps `balance ± (price · qty)`
  exact within `Decimal`'s 28-digit budget.
- **Per-entry CLOB seq lockstep**: every entry performs exactly one
  `clob.apply`/`apply_noop`, so perps and CLOB sequence counters move together.
- **State hash** (`lq-perps-v1`): config, sequences, collateral, positions
  (serde pair-seq — JSON cannot key structs), funding indexes, flags, stats,
  plus the embedded CLOB hash. Same log ⇒ byte-identical hash.

## Verification

- **23 behavioral tests** (`perps_behavior.rs`): deposit/withdraw boundaries,
  atomic fill + fee credit, insufficient-margin reject, `max_order_qty` Reduce,
  `max_notional`/`max_position`/`max_open_orders` rejects, pre-trade purity,
  reduce-only place-cap + auto-cancel (`CancelReason::ReduceOnly`), funding
  zero-sum + `no_mark_price`, book-fill liquidation (exact limit price and
  collateral asserts), insurance residual + ADL, healthy/no-position rejects,
  cascade breaker, legacy `Fill` no-op on subaccounts, seq-gap-is-the-only-`Err`,
  replay trio (double-run / empty-log rebuild / snapshot midway) + encode/decode
  roundtrip. The fixture asserts `check_invariants` after **every** entry.
- **proptest invariants** (`perps_invariants.rs`): seeded xorshift-generated
  64-entry logs across two markets (transfers, ticks, places, cancels,
  liquidations, legacy fills, funding) — invariants after every entry, double-
  run output/hash equality, encode/decode roundtrip; regression seeds pinned in
  `perps_invariants.proptest-regressions`.
- **Unit tests** (6): margin math, limit-price clamping, subaccount bookkeeping.
- **Benchmarks** (criterion, quick run on this machine):

  | Bench | mean |
  |---|---|
  | `apply_place_with_margin_check` | ~2.15 µs |
  | `liquidate_underwater_position` | ~648 ns |
  | `state_hash_2000_subaccounts` | ~253 µs |

## Tradeoffs

| Choice | Why | Cost |
|---|---|---|
| Cash-basis equity (`collateral ± qty·price`) | no per-position realized-PnL ledgers; conservation is checkable | mark must exist for equity; oracle-driven mark landed in Stage 4 |
| `PerpsState` composes `ClobState` (one machine) | fill + margin = one atomic transition, one hash | perps harness must run the CLOB seq lockstep |
| Liquidation = synthetic IOC + insurance residual | reuses matching, exact bankruptcy floor, no special fill engine | synthetic ids must stay out of the client id space (high-half prefix) |
| ADL closes largest opposite at insurance-neutral price | deterministic, terminates, no RNG/priority scores | simpler than dYdX's deleveraging priority ranking |
| Risk limits as pre-trade verdicts (incl. Reduce) | one choke point; `lq-risk` semantics preserved (`place_checked`) | limits live in state config → changing them needs replica agreement |
| Legacy external `Fill` doesn't move subaccounts | counterparty unknown in a solo entry; avoids fake conservation | external venues must be migrated to real fills or replayed per side |
| Rejections are outputs, not `Err` | WAL always replays; seq gaps remain the only `Err` | callers must check outputs |

## Residual risks / missing

- **Mark price is trade-based** (`last_trade` / tick): no index price or
  oracle median yet — delivered by Stage 4 (`lq-oracle`): log-driven oracle
  median with staleness/deviation gates (see `STAGE_4_ORACLE.md`).
- Funding is **operator-triggered** (`SettleFunding` per entry); interval
  scheduling and rate derivation come with the Stage 5 funding daemon (now
  oracle-gated).
- Liquidator daemon does not exist yet: `pending_liquidations` is consumed by
  tests; wiring a daemon to submit `Liquidate` entries is Stage 5/6 work.
- Insurance fund has no deposit/withdrawal entries of its own — it grows from
  fees and liquidation margins only.
- `sweep_flat_negatives` zeroes dust balances; no "dust threshold" policy yet.
- Open-order margin reservation assumes resting orders are all margin-bound
  at their limit price; correlation/portfolio effects are out of scope.
- Live engine still runs `EngineState`; `PerpsState` is exercised through the
  sequencer, not yet through the engine loop (cutover is Stage 5+).
