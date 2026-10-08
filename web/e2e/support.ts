import type { Page } from "@playwright/test";
import type { BookResponse, Inventory, LogEntry, LogResponse, MarketState, Order, Position, StateSummary } from "../src/types";

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

/** Serve a fixed book; the default is empty depth. */
export async function mockBook(page: Page, book: BookResponse = { seq: 0, bids: [], asks: [] }): Promise<void> {
  await page.route(/\/api\/v1\/book/, (route) => route.fulfill({ json: book }));
}

/** One WAL row with every nullable field defaulted. */
export function makeLogEntry(seq: number, kind: string, over: Partial<LogEntry> = {}): LogEntry {
  return {
    seq,
    kind,
    side: null,
    price: null,
    qty: null,
    tif: null,
    id: null,
    old_id: null,
    reject_reason: null,
    ...over,
  };
}

/** A complete /api/v1/log payload wrapping `entries` (head = last seq). */
export function makeLog(entries: LogEntry[], over: Partial<LogResponse> = {}): LogResponse {
  const head = entries.length ? entries[entries.length - 1].seq : 0;
  return {
    entries,
    head,
    replayed: head,
    state_hash: null,
    orders: [],
    position: "0",
    ...over,
  };
}

/** Serve a fixed order log; the default is an empty WAL. */
export async function mockLog(page: Page, log: LogResponse = makeLog([])): Promise<void> {
  await page.route(/\/api\/v1\/log/, (route) => route.fulfill({ json: log }));
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

/** The status line in the risk & control panel (the order form renders its own static #msg). */
export function msg(page: Page) {
  return page.locator('[aria-label="Risk and control"] #msg');
}

/** The connection banner shown while no snapshot has loaded. */
export function banner(page: Page) {
  return page.locator(".banner");
}

/** The value column of the meter labelled `label`. */
export function meter(page: Page, label: string) {
  return page.locator(`.meter:has-text("${label}") b`);
}

export function button(page: Page, name: string) {
  return page.getByRole("button", { name, exact: true });
}
