# Risk

The risk engine is the only authority between a strategy's decision and an
order on a venue. Every order passes through `RiskEngine::validate_order`
before placement; the result is one of:

| Decision | Meaning |
|---|---|
| `Allow` | Place as-is |
| `Reduce { qty }` | Place with a reduced quantity |
| `Reject { code, detail }` | Do not place |
| `Halt { code, detail }` | Do not place, engage the kill switch |

## Limits (`[risk]`)

| Setting | What it bounds |
|---|---|
| `max_position_qty` | Net absolute position per venue/symbol |
| `max_order_qty` | Single order quantity |
| `max_open_orders` | Concurrent working orders |
| `max_notional` | Notional of a single order (`price × qty`) |
| `max_daily_loss` | Realized + unrealized loss before halting |
| `max_order_rate_per_sec` | Order submissions per second |
| `max_exposure_per_venue` | Sum of working order notional per venue |
| `max_price_deviation_bps` | Order price vs. mark price deviation |

## Trip-wires and the kill switch

The kill switch halts all quoting and cancels working orders. It engages on:

- **Stale feed** — no market event for `stale_market_ms`
  (`kill_switch_on_stale`).
- **Venue reconnect** — order state is suspect after a disconnect
  (`kill_switch_on_reconnect`).
- **Loss limit** — `max_daily_loss` exceeded.
- **Manual** — `POST /api/v1/control/kill`.

Releasing it is an explicit operator action:
`POST /api/v1/control/reset`.

## Order state machine

Orders move through a validated state machine (see `OrderStateMachine`):

```
Created → Submitted → Acknowledged → Filled
                    ↘ Cancelled / Expired / Rejected
```

Illegal transitions (e.g. `Acknowledged → Submitted`) are rejected. Fills
update `filled_quantity`, `avg_fill_price`, and status; partial fills are
tracked until the remaining quantity is zero.

## Why risk is a separate layer

- Strategies are free to be naive; risk is the backstop.
- Every limit is testable in isolation (`crates/risk`).
- A new venue adapter cannot bypass risk: the engine calls `validate_order`
  in the single place where orders leave the process.

## Event-sourced path (`lq-perps`, Stage 3)

On the sequencer path the same limits are enforced **inside `apply`** as
pre-trade verdicts, so no order can reach the book unchecked:

- `pre_trade_check` evaluates validation, the `PerpsConfig` limits
  (`max_order_qty`, `max_position_qty`, `max_notional_per_order`,
  `max_open_orders` — `0` disables) and the initial/maintenance margin
  requirement, returning `Allow` / `Reduce` (cap the qty, `lq-risk`
  `place_checked` parity) / `Reject { code }` with a stable reason string.
- Maintenance margin is the liquidation threshold: below it the subaccount
  lands in the pending liquidation set; `Liquidate` closes it at the
  bankruptcy limit price against the book, with the insurance fund as
  backstop and ADL if the insurance fund goes negative.
- The legacy `RiskEngine` (this file's table above) still guards the
  in-process engine path only; the kill switch / `max_daily_loss` semantics
  are unchanged there until that path is retired (Stage 8).
