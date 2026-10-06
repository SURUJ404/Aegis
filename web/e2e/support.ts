import type { Page } from "@playwright/test";
import type { Inventory, MarketState, Order, Position, StateSummary } from "../src/types";

export function makeState(overrides: Partial<StateSummary> = {}): StateSummary {
  return {
    positions: [],
    inventory: [],
    orders: [],
    market_state: [],
    risk: { armed: true, halted: false, halt_reason: null, updated_at: 1_790_000_000_000 },
    strategy_running: false,
    uptime_ms: 42_000,
    ...overrides,
  };
}

export function makePosition(over: Partial<Position> = {}): Position {
  return {
    venue: "paper",
    symbol: "BTC-USDT",
    net_qty: "0.5",
    avg_entry: "63000.10",
    realized_pnl: "123.45",
    event_ts: 1_790_000_000_000,
    ...over,
  };
}

export function makeInventory(over: Partial<Inventory> = {}): Inventory {
  return {
    symbol: "BTC-USDT",
    net_qty: "1.5",
    avg_entry: "63100.00",
    realized_pnl: "250.00",
    event_ts: 1_790_000_000_000,
    ...over,
  };
}

export function makeOrder(over: Partial<Order> = {}): Order {
  return {
    order_id: "order-0001",
    client_order_id: "c-0001",
    venue_order_id: "v-0001",
    venue: "paper",
    symbol: "BTC-USDT",
    side: "bid",
    order_type: "limit",
    price: "64000.00",
    quantity: "0.25",
    filled_quantity: "0",
    avg_fill_price: null,
    status: "acknowledged",
    time_in_force: "gtc",
    created_at: 1_790_000_000_000,
    updated_at: 1_790_000_000_000,
    ...over,
  };
}

export function makeMarket(over: Partial<MarketState> = {}): MarketState {
  return {
    venue: "paper",
    symbol: "BTC-USDT",
    event_ts: 1_790_000_000_000,
    best_bid: "64000.00",
    best_ask: "64001.00",
    mid: "64000.50",
    spread: "1.00",
    spread_bps: 0.16,
    orderbook_imbalance: 0.12,
    microprice: "64000.60",
    vwap: "64000.20",
    depth_bid: "12.5",
    depth_ask: "9.75",
    num_bid_levels: 10,
    num_ask_levels: 10,
    buy_volume: "120.5",
    sell_volume: "95.25",
    trade_intensity: 3.4,
    realized_volatility: 0.018,
    price_impact_estimate: 0.0004,
    regime: "normal",
    stale: false,
    ...over,
  };
}

/** Serve a fixed /api/v1/state payload to every poll. */
export async function mockState(page: Page, state: StateSummary): Promise<void> {
  await page.route("**/api/v1/state", (route) => route.fulfill({ json: state }));
}

/** Serve fixed control responses and optionally capture request bodies. */
export async function mockControl(
  page: Page,
  response: { accepted: boolean; message: string } = { accepted: true, message: "published" },
  delayMs = 0,
): Promise<void> {
  await page.route("**/api/v1/control/*", async (route) => {
    if (delayMs > 0) await new Promise((r) => setTimeout(r, delayMs));
    await route.fulfill({ json: response });
  });
}

/** The stat card whose label matches `label`. */
export function statValue(page: Page, label: string) {
  return page.locator(`.stat:has-text("${label}") .stat-value`);
}

export function button(page: Page, name: string) {
  return page.getByRole("button", { name, exact: true });
}
