# Development

Guidelines, layout, and the pitfalls worth knowing before changing code.

## Workspace layout

```
Cargo.toml            workspace manifest (deps, lints, profiles)
crates/               libraries, one responsibility each
apps/                 binaries composing the crates
docker/               Dockerfile + docker-compose stack
deploy/               Kubernetes manifests
docs/                 this documentation set
benches/              (placeholder for workspace-level benches)
tests/                (placeholder for end-to-end tests)
```

## Build / test / lint / bench

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
cargo bench --workspace
```

`cargo bench` benchmarks live in each crate (`crates/*/benches`):

- `lq-orderbook`: snapshot apply, delta apply, analytics compute
- `lq-market-data`: Binance 20-level depth decode
- `lq-strategy`: market-making decision cost
- `lq-risk`: order validation

## Conventions

- `cargo fmt` style, edition 2021.
- No `unsafe` anywhere: `[workspace.lints.rust] unsafe_code = "forbid"`.
- Every limit/knob is explicit and lives in `EngineConfig`; nothing is magic.
- New strategy/risk logic must not depend on networking or databases.
- All domain models are `Serialize`/`Deserialize` where the API or persistence
  touches them.
- Add a test for every fix (see the crate test suites) and keep backtests
  deterministic.

## Deterministic backtests

A backtest must be byte-identical run-to-run. The guarantees that make this
hold:

- The synthetic market and the paper venue share a single seeded RNG.
- Rejection is forced to 0 and latency is disabled for the venue.
- The venue's bus publishing is disabled (`PaperExecutionVenue` created with
  `.with_publishing(false)` in the runner), so fills are applied
  synchronously in the engine loop instead of arriving via the async
  `EventBus` fan-out. The bus broker's delivery timing is not deterministic,
  so relying on it would perturb inventory updates and cascade into different
  quotes and fill prices.

Preserve these properties when touching `lq-backtest`, `lq-execution` or
`lq-simulator`.

## Pitfalls

- **`EventBus::new()` spawns broker tasks** and must run inside a Tokio
  runtime. Outside one it panics with "there is no reactor running". Tests
  must be `#[tokio::test]`; binaries run inside `#[tokio::main]` or an
  explicit runtime.
- **Windows PowerShell `-replace`** treats replacement strings literally and
  does not expand `\n`. Use the editor tooling for multi-line edits.
- **`Decimal` is not `Copy`**; pass by reference. `Decimal::as_f64()` returns
  `f64` directly in rust_decimal 1.42. `ln`/`powi` need the `maths` feature
  (`MathematicalOps` trait).
- **Maker rebates make `fees_total` negative.** Assert `fees_total != 0`, not
  `> 0`.
- **`PaperExchange` is `!Send`.** Run it on a current-thread runtime inside a
  `LocalSet` and use `spawn_local`, not `tokio::spawn`.
- **`Symbol` wraps `String`** — it cannot be used in a `const`. Use a helper
  fn (`Symbol("BTC-USDT".into())`).

## Adding a venue or strategy

1. Venue adapter: implement `FeedDecoder` in `crates/market-data`, add the
   `Exchange` variant in `crates/types`.
2. Strategy: implement `Strategy`, register it in `StrategyEngine`, add a
   config section under `StrategyConfig`.
3. Wire it in the binary that needs it (engine / market-data-service).
4. Add decoder + strategy tests; keep determinism.
