//! Application state, background polling and key handling.

use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{
    Backtest, BacktestReq, ControlResponse, EngineClient, Prices, QuantClient, QuantHealth,
    ScreenRow, StateSummary, STRATEGIES,
};

pub const TABS: [&str; 4] = ["Engine", "Screener", "Backtest", "Market"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Engine,
    Screener,
    Backtest,
    Market,
}

impl Tab {
    pub fn index(self) -> usize {
        match self {
            Tab::Engine => 0,
            Tab::Screener => 1,
            Tab::Backtest => 2,
            Tab::Market => 3,
        }
    }

    fn from_index(i: usize) -> Self {
        match i % TABS.len() {
            0 => Tab::Engine,
            1 => Tab::Screener,
            2 => Tab::Backtest,
            _ => Tab::Market,
        }
    }
}

/// Everything the background tasks send back to the UI loop.
pub enum Msg {
    EngineState(Result<StateSummary, String>),
    QuantHealth(Result<QuantHealth, String>),
    Symbols(Result<Vec<String>, String>),
    Screen(Result<Vec<ScreenRow>, String>),
    Backtest(Result<Backtest, String>),
    Prices(Result<Prices, String>),
    Control(Result<ControlResponse, String>),
}

#[derive(Default)]
pub struct EngineView {
    pub state: Option<StateSummary>,
    pub up: bool,
    pub error: Option<String>,
    pub order_sel: usize,
    pub last_control: String,
}

pub struct QuantView {
    pub health: Option<QuantHealth>,
    pub up: bool,
    pub error: Option<String>,
    pub symbols: Vec<String>,
    // screener
    pub screen: Vec<ScreenRow>,
    pub screen_strategy: usize,
    pub screen_a: i32,
    pub screen_b: i32,
    pub screen_sel: usize,
    pub screen_busy: bool,
    // backtest form
    pub bt_symbol: usize,
    pub bt_strategy: usize,
    pub bt_a: i32,
    pub bt_b: i32,
    pub bt_cost_bps: f64,
    pub bt_field: usize,
    pub bt_busy: bool,
    pub bt_result: Option<Backtest>,
    pub bt_error: Option<String>,
    // market
    pub mk_symbol: usize,
    pub mk_days: i32,
    pub mk_field: usize,
    pub mk_prices: Option<Prices>,
    pub mk_busy: bool,
}

impl Default for QuantView {
    fn default() -> Self {
        Self {
            health: None,
            up: false,
            error: None,
            symbols: Vec::new(),
            screen: Vec::new(),
            screen_strategy: 0,
            screen_a: 20,
            screen_b: 60,
            screen_sel: 0,
            screen_busy: false,
            bt_symbol: 0,
            bt_strategy: 0,
            bt_a: 20,
            bt_b: 60,
            bt_cost_bps: 10.0,
            bt_field: 0,
            bt_busy: false,
            bt_result: None,
            bt_error: None,
            mk_symbol: 0,
            mk_days: 120,
            mk_field: 0,
            mk_prices: None,
            mk_busy: false,
        }
    }
}

pub struct App {
    pub quit: bool,
    pub tab: Tab,
    pub status: String,
    /// `Some(text)` while the kill-reason prompt is open.
    pub kill_input: Option<String>,
    pub engine: EngineView,
    pub quant: QuantView,
    engine_client: EngineClient,
    quant_client: QuantClient,
    tx: UnboundedSender<Msg>,
}

impl App {
    pub fn new(engine_client: EngineClient, quant_client: QuantClient, tx: UnboundedSender<Msg>) -> Self {
        Self {
            quit: false,
            tab: Tab::Engine,
            status: "ready — F1 shows key help in the footer".into(),
            kill_input: None,
            engine: EngineView::default(),
            quant: QuantView::default(),
            engine_client,
            quant_client,
            tx,
        }
    }

    // -------------------------------------------------------------- messages

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::EngineState(Ok(s)) => {
                self.engine.up = true;
                self.engine.error = None;
                if let Some(o) = self.engine.state.as_ref().map(|s| s.orders.len()) {
                    if self.engine.order_sel >= o {
                        self.engine.order_sel = o.saturating_sub(1);
                    }
                }
                self.engine.state = Some(s);
            }
            Msg::EngineState(Err(e)) => {
                self.engine.up = false;
                self.engine.error = Some(e);
            }
            Msg::QuantHealth(Ok(h)) => {
                self.quant.up = h.ok;
                self.quant.error = None;
                self.quant.health = Some(h);
            }
            Msg::QuantHealth(Err(e)) => {
                self.quant.up = false;
                self.quant.error = Some(e);
            }
            Msg::Symbols(Ok(s)) => {
                self.quant.symbols = s;
                self.status = format!("loaded {} symbols", self.quant.symbols.len());
            }
            Msg::Symbols(Err(e)) => self.status = format!("symbols: {e}"),
            Msg::Screen(Ok(rows)) => {
                self.quant.screen_busy = false;
                if rows.is_empty() {
                    self.status = "screener returned no rows".into();
                } else {
                    self.status = format!("screener: {} rows", rows.len());
                }
                self.quant.screen = rows;
                if self.quant.screen_sel >= self.quant.screen.len() {
                    self.quant.screen_sel = self.quant.screen.len().saturating_sub(1);
                }
            }
            Msg::Screen(Err(e)) => {
                self.quant.screen_busy = false;
                self.status = format!("screen failed: {e}");
            }
            Msg::Backtest(Ok(b)) => {
                self.quant.bt_busy = false;
                self.quant.bt_error = None;
                self.status = format!(
                    "backtest {} — cagr {:.2}%, sharpe {:.2}",
                    b.symbol,
                    b.strategy.cagr * 100.0,
                    b.strategy.sharpe
                );
                self.quant.bt_result = Some(b);
            }
            Msg::Backtest(Err(e)) => {
                self.quant.bt_busy = false;
                self.quant.bt_error = Some(e.clone());
                self.status = format!("backtest failed: {e}");
            }
            Msg::Prices(Ok(p)) => {
                self.quant.mk_busy = false;
                self.status = format!("{}: {} bars", p.symbol, p.bars.len());
                self.quant.mk_prices = Some(p);
            }
            Msg::Prices(Err(e)) => {
                self.quant.mk_busy = false;
                self.status = format!("prices failed: {e}");
            }
            Msg::Control(Ok(r)) => {
                let mark = if r.accepted { "ok" } else { "rejected" };
                self.status = format!("control {mark}: {}", r.message);
                self.engine.last_control = format!("{mark}: {}", r.message);
            }
            Msg::Control(Err(e)) => {
                self.status = format!("control failed: {e}");
                self.engine.last_control = format!("failed: {e}");
            }
        }
    }

    // --------------------------------------------------------------- actions

    fn control(&mut self, action: &'static str) {
        let client = self.engine_client.clone();
        let tx = self.tx.clone();
        self.status = format!("control {action}…");
        tokio::spawn(async move {
            let _ = tx.send(Msg::Control(client.control(action).await));
        });
    }

    fn kill(&mut self, reason: String) {
        let client = self.engine_client.clone();
        let tx = self.tx.clone();
        self.status = format!("kill switch: {reason}");
        tokio::spawn(async move {
            let _ = tx.send(Msg::Control(client.kill(&reason).await));
        });
    }

    pub fn request_screen(&mut self) {
        if self.quant.screen_busy {
            return;
        }
        let strategy = STRATEGIES[self.quant.screen_strategy];
        let (a, b) = (self.quant.screen_a, self.quant.screen_b);
        if ma_invalid(strategy, a, b) {
            self.status = "rejected: for ma, fast days (a) must be shorter than slow days (b)".into();
            return;
        }
        let client = self.quant_client.clone();
        let tx = self.tx.clone();
        self.quant.screen_busy = true;
        self.status = "screening…".into();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Screen(client.screen(strategy, a, b).await));
        });
    }

    pub fn request_backtest(&mut self) {
        if self.quant.bt_busy {
            return;
        }
        let symbol = self
            .quant
            .symbols
            .get(self.quant.bt_symbol)
            .cloned()
            .unwrap_or_default();
        if symbol.is_empty() {
            self.status = "no symbols loaded yet — is the quant API up?".into();
            return;
        }
        let strategy = STRATEGIES[self.quant.bt_strategy];
        if ma_invalid(strategy, self.quant.bt_a, self.quant.bt_b) {
            self.quant.bt_error = Some("for ma, fast days (a) must be shorter than slow days (b)".into());
            self.status = "rejected: a must be shorter than b for ma".into();
            return;
        }
        let req = BacktestReq {
            symbol,
            strategy: strategy.to_string(),
            a: self.quant.bt_a,
            b: self.quant.bt_b,
            cost_bps: self.quant.bt_cost_bps,
        };
        let client = self.quant_client.clone();
        let tx = self.tx.clone();
        self.quant.bt_busy = true;
        self.quant.bt_error = None;
        self.status = format!("backtesting {}…", req.symbol);
        tokio::spawn(async move {
            let _ = tx.send(Msg::Backtest(client.backtest(&req).await));
        });
    }

    pub fn request_prices(&mut self) {
        if self.quant.mk_busy {
            return;
        }
        let symbol = self
            .quant
            .symbols
            .get(self.quant.mk_symbol)
            .cloned()
            .unwrap_or_default();
        if symbol.is_empty() {
            self.status = "no symbols loaded yet — is the quant API up?".into();
            return;
        }
        let days = self.quant.mk_days;
        let client = self.quant_client.clone();
        let tx = self.tx.clone();
        self.quant.mk_busy = true;
        self.status = format!("loading {symbol}…");
        tokio::spawn(async move {
            let _ = tx.send(Msg::Prices(client.prices(&symbol, days).await));
        });
    }

    // ----------------------------------------------------------------- keys

    pub fn on_key(&mut self, key: KeyEvent) {
        if let Some(buf) = self.kill_input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.kill_input = None;
                    self.status = "kill cancelled".into();
                }
                KeyCode::Enter => {
                    let reason = if buf.trim().is_empty() {
                        "manual halt from TUI".to_string()
                    } else {
                        buf.clone()
                    };
                    self.kill_input = None;
                    self.kill(reason);
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => {
                let next = Tab::from_index(self.tab.index() + 1);
                self.tab = next;
            }
            KeyCode::BackTab => {
                let prev = Tab::from_index(self.tab.index() + TABS.len() - 1);
                self.tab = prev;
            }
            KeyCode::Char('1') => self.tab = Tab::Engine,
            KeyCode::Char('2') => self.tab = Tab::Screener,
            KeyCode::Char('3') => self.tab = Tab::Backtest,
            KeyCode::Char('4') => self.tab = Tab::Market,
            _ => self.on_tab_key(key),
        }
    }

    fn on_tab_key(&mut self, key: KeyEvent) {
        match self.tab {
            Tab::Engine => self.engine_key(key),
            Tab::Screener => self.screener_key(key),
            Tab::Backtest => self.backtest_key(key),
            Tab::Market => self.market_key(key),
        }
    }

    fn engine_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('s') => self.control("start"),
            KeyCode::Char('p') => self.control("stop"),
            KeyCode::Char('r') => self.control("reset"),
            KeyCode::Char('k') => {
                self.kill_input = Some(String::new());
                self.status = "kill reason: type and press Enter (Esc cancels)".into();
            }
            KeyCode::Up => {
                let len = self.engine.state.as_ref().map(|s| s.orders.len()).unwrap_or(0);
                self.engine.order_sel = self.engine.order_sel.saturating_sub(1).min(len.saturating_sub(1));
            }
            KeyCode::Down => {
                let len = self.engine.state.as_ref().map(|s| s.orders.len()).unwrap_or(0);
                if len > 0 {
                    self.engine.order_sel = (self.engine.order_sel + 1).min(len - 1);
                }
            }
            _ => {}
        }
    }

    fn screener_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => {
                self.quant.screen_strategy = (self.quant.screen_strategy + STRATEGIES.len() - 1) % STRATEGIES.len();
            }
            KeyCode::Right => {
                self.quant.screen_strategy = (self.quant.screen_strategy + 1) % STRATEGIES.len();
            }
            KeyCode::Char('[') => self.quant.screen_a = (self.quant.screen_a - 1).max(2),
            KeyCode::Char(']') => self.quant.screen_a = (self.quant.screen_a + 1).min(250),
            KeyCode::Char('-') => self.quant.screen_b = (self.quant.screen_b - 1).max(2),
            KeyCode::Char('=') => self.quant.screen_b = (self.quant.screen_b + 1).min(250),
            KeyCode::PageUp => {
                self.quant.screen_a = (self.quant.screen_a - 10).max(2);
                self.quant.screen_b = (self.quant.screen_b - 10).max(2);
            }
            KeyCode::PageDown => {
                self.quant.screen_a = (self.quant.screen_a + 10).min(250);
                self.quant.screen_b = (self.quant.screen_b + 10).min(250);
            }
            KeyCode::Up => {
                self.quant.screen_sel = self.quant.screen_sel.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.quant.screen.is_empty() {
                    self.quant.screen_sel = (self.quant.screen_sel + 1).min(self.quant.screen.len() - 1);
                }
            }
            KeyCode::Enter | KeyCode::Char('R') => self.request_screen(),
            _ => {}
        }
    }

    fn backtest_key(&mut self, key: KeyEvent) {
        const FIELDS: usize = 6;
        match key.code {
            KeyCode::Up => self.quant.bt_field = (self.quant.bt_field + FIELDS - 1) % FIELDS,
            KeyCode::Down => self.quant.bt_field = (self.quant.bt_field + 1) % FIELDS,
            KeyCode::Enter => self.request_backtest(),
            KeyCode::Left | KeyCode::Right => {
                let right = key.code == KeyCode::Right;
                let n = self.quant.symbols.len().max(1);
                match self.quant.bt_field {
                    0 => {
                        self.quant.bt_symbol = if right {
                            (self.quant.bt_symbol + 1) % n
                        } else {
                            (self.quant.bt_symbol + n - 1) % n
                        };
                    }
                    1 => {
                        self.quant.bt_strategy = if right {
                            (self.quant.bt_strategy + 1) % STRATEGIES.len()
                        } else {
                            (self.quant.bt_strategy + STRATEGIES.len() - 1) % STRATEGIES.len()
                        };
                    }
                    2 => self.quant.bt_a = step(self.quant.bt_a, right, 1, 2, 250),
                    3 => self.quant.bt_b = step(self.quant.bt_b, right, 1, 2, 250),
                    4 => {
                        self.quant.bt_cost_bps = (self.quant.bt_cost_bps + if right { 5.0 } else { -5.0 }).clamp(0.0, 200.0)
                    }
                    _ => self.request_backtest(),
                }
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let up = key.code == KeyCode::PageUp;
                match self.quant.bt_field {
                    2 => self.quant.bt_a = step(self.quant.bt_a, !up, 10, 2, 250),
                    3 => self.quant.bt_b = step(self.quant.bt_b, !up, 10, 2, 250),
                    4 => {
                        self.quant.bt_cost_bps =
                            (self.quant.bt_cost_bps + if up { -10.0 } else { 10.0 }).clamp(0.0, 200.0)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn market_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                self.quant.mk_field = 1 - self.quant.mk_field.min(1);
            }
            KeyCode::Left | KeyCode::Right => {
                let right = key.code == KeyCode::Right;
                let n = self.quant.symbols.len().max(1);
                if self.quant.mk_field == 0 {
                    self.quant.mk_symbol = if right {
                        (self.quant.mk_symbol + 1) % n
                    } else {
                        (self.quant.mk_symbol + n - 1) % n
                    };
                } else {
                    self.quant.mk_days = step(self.quant.mk_days, right, 5, 5, 1000);
                }
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                if self.quant.mk_field == 1 {
                    let up = key.code == KeyCode::PageUp;
                    self.quant.mk_days = step(self.quant.mk_days, !up, 30, 5, 1000);
                }
            }
            KeyCode::Enter | KeyCode::Char('R') => self.request_prices(),
            _ => {}
        }
    }
}

fn step(v: i32, up: bool, by: i32, min: i32, max: i32) -> i32 {
    if up {
        (v + by).min(max)
    } else {
        (v - by).max(min)
    }
}

/// Mirror of the API's `BacktestRequest` validation: for `ma`, the fast
/// average must be strictly shorter than the slow one.
fn ma_invalid(strategy: &str, a: i32, b: i32) -> bool {
    strategy == "ma" && a >= b
}

// ------------------------------------------------------------- poll tasks

/// Spawn the always-on pollers: engine state (1s), quant health (5s) and the
/// one-shot symbol + screener warm-up.
pub fn spawn_pollers(engine: EngineClient, quant: QuantClient, tx: UnboundedSender<Msg>) {
    {
        let engine = engine.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if tx.send(Msg::EngineState(engine.state().await)).is_err() {
                    break;
                }
            }
        });
    }

    {
        let quant = quant.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if tx.send(Msg::QuantHealth(quant.health().await)).is_err() {
                    break;
                }
            }
        });
    }

    tokio::spawn(async move {
        if tx.send(Msg::Symbols(quant.symbols().await)).is_err() {
            return;
        }
        let _ = tx.send(Msg::Screen(quant.screen("ma", 20, 60).await));
    });
}
