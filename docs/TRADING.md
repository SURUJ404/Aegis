# Trading

Strategy, execution, and accounting semantics. Read this before changing
anything that touches an order or a PnL number.

## Strategy layer

A `Strategy` observes a `StrategyContext` (market state, per-symbol inventory,
per-venue position, halt/running flags) and returns a single `StrategyDecision`:

- `Quote(QuoteIntent)` — replace the two-sided quote for a symbol/venue
- `MarketOrder(MarketOrderSignal)` — take liquidity now
- `StandDown { reason }` — cancel working orders, wait
- `Hold` — do nothing

Strategies are **pure**: no networking, no database, no bus access. They get
everything through the context, which makes them deterministic (the backbone
of backtesting) and trivially unit-testable.

### Baseline market-making strategy

`MarketMakingStrategy` quotes both sides around the mid:

```
bid_price = mid × (1 − half_spread × (1 + skew))
ask_price = mid × (1 + half_spread × (1 − skew))
```

- `half_spread` adapts to volatility (`vol_scale_half_spread`) within
  `min_spread_bps` / `max_spread_bps`.
- `skew` is a function of inventory vs. `inventory_target_qty` and
  `inventory_max_qty`: long inventory widens the bid and narrows the ask
  (we want to sell).
- Order-book imbalance further offsets each side (we quote closer to the
  larger resting side).
- Quotes refresh at most every `quote_refresh_ms`; each refresh cancels the
  previous quote and re-places, so at most 2 working orders per symbol/venue
  at any instant.

## Execution layer

`PaperExecutionVenue` is the reference implementation of `ExecutionVenue`:

- **Latency**: `base_latency_ms` + uniform `latency_jitter_ms` (disabled in
  backtests).
- **Fill model**: when the market crosses a resting order, it fills with
  probability `fill_fraction` (times queue-position weighting), and may be a
  partial fill (`partial_fill_prob`, `partial_fill_fraction`).
- **Rejection**: `reject_prob` (forced to 0 in backtests).
- **Market orders** price against the venue's live touch with
  `slippage_bps`.

## Fee and PnL accounting

Fees are charged on every fill, from the venue's perspective:

- Taker fills pay `fee_rate_bps` (positive fee).
- Maker fills earn `maker_rebate_bps` (negative fee — a rebate).

**Opening fills** (increasing the net position) charge their fee against
realized PnL immediately. **Closing fills** (reducing/round-tripping the net
position) realize PnL `(exit − entry) × qty` on the closed quantity and charge
their fee as well. Over a round trip both opening and closing fees are netted,
so total PnL = gross spread − total fees.

Because maker rebates can exceed taker-side fees, `fees_total` may be negative
(you were paid). Tests assert `fees_total != 0`, not `fees_total > 0`.

Mark-to-market: `EquitySample` in backtests marks the residual position at the
latest mid. The `lq_execution::positions` module is the **system of record**
for positions, inventory, and realized PnL; the engine mirrors it into
`EngineState` for the API and metrics.

## Inventory vs. position

- **Position** is per (venue, symbol): `net_qty`, `avg_entry`, `realized_pnl`.
- **Inventory** is per symbol, **aggregated across venues**: strategies see
  inventory so they don't build a net exposure across venues; they see the
  per-venue position for venue-specific skew.
