export type Exchange = "paper" | "simulated" | "okx" | "binance" | "bybit";
export type Side = "bid" | "ask";
export type OrderStatus =
  | "created"
  | "submitted"
  | "acknowledged"
  | "partially_filled"
  | "filled"
  | "cancel_requested"
  | "cancelled"
  | "rejected"
  | "expired";
export type OrderType =
  | "limit"
  | "market"
  | "post_only"
  | "immediate_or_cancel"
  | "fill_or_kill";
export type TimeInForce = "gtc" | "ioc" | "fok" | "post_only";
export type Regime =
  | "normal"
  | "volatile"
  | "one_sided_bid"
  | "one_sided_ask"
  | "stale"
  | "no_liquidity";

export interface Order {
  order_id: string;
  client_order_id: string;
  venue_order_id: string;
  venue: Exchange;
  symbol: string;
  side: Side;
  order_type: OrderType;
  price: string | null;
  quantity: string;
  filled_quantity: string;
  avg_fill_price: string | null;
  status: OrderStatus;
  time_in_force: TimeInForce;
  created_at: number;
  updated_at: number;
}

export interface Position {
  venue: Exchange;
  symbol: string;
  net_qty: string;
  avg_entry: string;
  realized_pnl: string;
  event_ts: number;
}

export interface Inventory {
  symbol: string;
  net_qty: string;
  avg_entry: string;
  realized_pnl: string;
  event_ts: number;
}

export interface MarketState {
  venue: Exchange;
  symbol: string;
  event_ts: number;
  best_bid: string;
  best_ask: string;
  mid: string;
  spread: string;
  spread_bps: number;
  orderbook_imbalance: number;
  microprice: string;
  vwap: string;
  depth_bid: string;
  depth_ask: string;
  num_bid_levels: number;
  num_ask_levels: number;
  buy_volume: string;
  sell_volume: string;
  trade_intensity: number;
  realized_volatility: number;
  price_impact_estimate: number;
  regime: Regime;
  stale: boolean;
}

export interface RiskStatus {
  armed: boolean;
  halted: boolean;
  halt_reason: string | null;
  updated_at: number;
}

export interface StateSummary {
  positions: Position[];
  inventory: Inventory[];
  orders: Order[];
  market_state: MarketState[];
  risk: RiskStatus;
  strategy_running: boolean;
  uptime_ms: number;
}

export interface ControlResponse {
  accepted: boolean;
  message: string;
}

/** `GET /api/v1/book` — aggregated depth from the live engine books. */
export interface BookLevel {
  price: string;
  qty: string;
}

export interface BookResponse {
  seq: number;
  bids: BookLevel[];
  asks: BookLevel[];
}

/** One sequencer WAL entry as flattened by `GET /api/v1/log`. */
export interface LogEntry {
  seq: number;
  kind: string;
  side: Side | null;
  price: string | null;
  qty: string | null;
  tif: string | null;
  id: string | null;
  /** Cancelled order of a `replace_order` row (null for every other kind). */
  old_id: string | null;
  reject_reason: string | null;
}

/** An open order as of the snapshot sequence. */
export interface SnapshotOrder {
  order_id: string;
  side: Side;
  price: string | null;
  remaining: string;
  resting: boolean;
}

export interface LogResponse {
  entries: LogEntry[];
  head: number;
  /**
   * Sequence the state fields below reflect. Equals `head` once the server's
   * replay has caught up (immediately for a small WAL, after a background
   * catch-up for a long one); the dashboard rebuilds any sequence itself.
   */
  replayed: number;
  /**
   * State hash of the replayed prefix — only present when the caller asks for
   * it (`hash=1`): hashing walks every order the state holds, and the
   * dashboard rebuilds its own hash with the shared fold.
   */
  state_hash: string | null;
  /** Open orders as of `replayed` (server's own view; the UI folds its own). */
  orders: SnapshotOrder[];
  /** Net base position across markets as of `replayed`. */
  position: string;
}

/** What one poll of the control plane produces. */
export interface Snapshot {
  state: StateSummary;
  book: BookResponse | null;
  log: LogResponse | null;
}