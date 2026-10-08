//! Rendering. Pure function of [`App`] — no IO, no mutation.

use chrono::{DateTime, Utc};
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph, Row, Table, Tabs};
use ratatui::Frame;

use crate::api::{Backtest, Prices};
use crate::app::{App, Tab, TABS};

const CYAN: Color = Color::Cyan;
const GREEN: Color = Color::Green;
const RED: Color = Color::Red;
const YELLOW: Color = Color::Yellow;
const DIM: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let rows = ratatui::layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, rows[0]);
    draw_tabs(frame, app, rows[1]);
    match app.tab {
        Tab::Engine => draw_engine(frame, app, rows[2]),
        Tab::Screener => draw_screener(frame, app, rows[2]),
        Tab::Backtest => draw_backtest(frame, app, rows[2]),
        Tab::Market => draw_market(frame, app, rows[2]),
    }
    draw_status(frame, app, rows[3]);
    draw_keys(frame, app, rows[4]);
}

// ----------------------------------------------------------------- chrome

fn dot(up: bool) -> Span<'static> {
    Span::styled("●", Style::default().fg(if up { GREEN } else { RED }))
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let q = &app.quant;
    let quant_src = q
        .health
        .as_ref()
        .map(|h| h.data_source.clone())
        .unwrap_or_else(|| "—".into());
    let (pos, orders) = app
        .engine
        .state
        .as_ref()
        .map(|s| (s.positions.len(), s.orders.len()))
        .unwrap_or((0, 0));
    let uptime = app
        .engine
        .state
        .as_ref()
        .map(|s| fmt_uptime(s.uptime_ms))
        .unwrap_or_else(|| "—".into());

    let line = Line::from(vec![
        Span::styled(" Aegis ", Style::default().fg(Color::Black).bg(CYAN).add_modifier(Modifier::BOLD)),
        Span::raw("  engine "),
        dot(app.engine.up),
        Span::raw(if app.engine.up { " up" } else { " down" }),
        Span::raw("   quant "),
        dot(q.up),
        Span::raw(format!(" up ({quant_src})")),
        Span::raw(format!("   uptime {uptime}")),
        Span::raw(format!("   pos {pos}")),
        Span::raw(format!("   orders {orders}")),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_tabs(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!("{} {}", i + 1, t)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .style(Style::default().fg(DIM))
        .highlight_style(Style::default().fg(Color::Black).bg(CYAN).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (text, style) = match &app.kill_input {
        Some(buf) => (
            format!("  kill reason: {buf}▌   (Enter submits, Esc cancels)"),
            Style::default().fg(Color::Black).bg(RED),
        ),
        None => (
            format!("  {}", app.status),
            Style::default().fg(DIM),
        ),
    };
    frame.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_keys(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let hint = match app.tab {
        Tab::Engine => "s start  p stop  r reset  k kill  ↑↓ orders  1-4/Tab tabs  q quit",
        Tab::Screener => "←→ strategy  [ ] a  - = b  PgUp/PgDn ±10  ↑↓ row  Enter refresh  q quit",
        Tab::Backtest => "↑↓ field  ←→ value  PgUp/PgDn coarse  Enter run  q quit",
        Tab::Market => "↑↓ field  ←→ value  Enter fetch  q quit",
    };
    frame.render_widget(
        Paragraph::new(format!("  {hint}")).style(Style::default().fg(DIM)),
        area,
    );
}

// ----------------------------------------------------------------- engine

fn draw_engine(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = ratatui::layout::Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(5),
        Constraint::Min(7),
    ])
    .split(area);

    let top = ratatui::layout::Layout::horizontal([Constraint::Length(40), Constraint::Min(30)])
        .split(rows[0]);
    draw_risk(frame, app, top[0]);
    draw_summary(frame, app, top[1]);

    let state = match app.engine.state.as_ref() {
        Some(s) => s,
        None => {
            let msg = match &app.engine.error {
                Some(e) => format!("engine API unreachable: {e}"),
                None => "waiting for engine state…".to_string(),
            };
            frame.render_widget(placeholder(&msg), rows[1]);
            frame.render_widget(placeholder("no orders"), rows[2]);
            return;
        }
    };

    // positions
    if state.positions.is_empty() {
        frame.render_widget(placeholder("no positions"), rows[1]);
    } else {
        let header = Row::new(["Venue", "Symbol", "Net qty", "Avg entry", "Realized pnl"])
            .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
        let body = state.positions.iter().map(|p| {
            Row::new(vec![
                p.venue.to_string(),
                p.symbol.to_string(),
                p.net_qty.to_string(),
                p.avg_entry.to_string(),
                p.realized_pnl.to_string(),
            ])
        });
        let table = Table::new(
            body,
            [
                Constraint::Length(10),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Length(16),
            ],
        )
        .header(header)
        .block(Block::bordered().title(" Positions "));
        frame.render_widget(table, rows[1]);
    }

    // orders
    if state.orders.is_empty() {
        frame.render_widget(placeholder("no orders"), rows[2]);
        return;
    }
    let header = Row::new(["Id", "Symbol", "Side", "Type", "Qty", "Filled", "Price", "Status"])
        .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
    let body = state.orders.iter().map(|o| {
        let status_style = if o.status.is_terminal() {
            match o.status.as_str() {
                "filled" => Style::default().fg(GREEN),
                "cancelled" | "expired" => Style::default().fg(YELLOW),
                _ => Style::default().fg(RED),
            }
        } else {
            Style::default().fg(CYAN)
        };
        let price = o
            .price
            .map(|p| p.to_string())
            .unwrap_or_else(|| o.avg_fill_price.map(|p| p.to_string()).unwrap_or_else(|| "mkt".into()));
        Row::new(vec![
            Span::raw(o.order_id.to_string()[..8].to_string()),
            Span::raw(o.symbol.to_string()),
            Span::raw(o.side.as_str().to_string()),
            Span::raw(format!("{:?}", o.order_type).to_lowercase()),
            Span::raw(o.quantity.to_string()),
            Span::raw(o.filled_quantity.to_string()),
            Span::raw(price),
            Span::styled(o.status.as_str().to_string(), status_style),
        ])
    });
    let table = Table::new(
        body,
        [
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(16),
        ],
    )
    .header(header)
    .block(Block::bordered().title(" Orders "))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");
    let mut st = ratatui::widgets::TableState::default()
        .with_selected(Some(app.engine.order_sel.min(state.orders.len() - 1)));
    frame.render_stateful_widget(table, rows[2], &mut st);
}

fn draw_risk(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    match app.engine.state.as_ref() {
        Some(s) => {
            let r = &s.risk;
            lines.push(kv("armed", if r.armed { "yes" } else { "NO" }, if r.armed { GREEN } else { RED }));
            lines.push(kv(
                "halted",
                if r.halted { "YES" } else { "no" },
                if r.halted { RED } else { GREEN },
            ));
            lines.push(kv(
                "reason",
                r.halt_reason.clone().unwrap_or_else(|| "—".into()),
                Color::White,
            ));
            lines.push(kv("updated", fmt_ts(r.updated_at.as_u64()), DIM));
        }
        None => lines.push(Line::from(Span::styled("no engine state", Style::default().fg(DIM)))),
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Risk ")),
        area,
    );
}

fn draw_summary(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    match app.engine.state.as_ref() {
        Some(s) => {
            lines.push(kv(
                "strategy",
                if s.strategy_running { "running" } else { "stopped" },
                if s.strategy_running { GREEN } else { YELLOW },
            ));
            lines.push(kv("uptime", fmt_uptime(s.uptime_ms), Color::White));
            lines.push(Line::from(format!(
                "counts    pos {}  inv {}  orders {}  market {}",
                s.positions.len(),
                s.inventory.len(),
                s.orders.len(),
                s.market_state.len()
            )));
        }
        None => lines.push(Line::from(Span::styled("no engine state", Style::default().fg(DIM)))),
    }
    lines.push(Line::from(vec![
        Span::raw("control   "),
        Span::styled(
            if app.engine.last_control.is_empty() {
                "—".to_string()
            } else {
                app.engine.last_control.clone()
            },
            Style::default().fg(CYAN),
        ),
    ]));
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Engine ")),
        area,
    );
}

// --------------------------------------------------------------- screener

fn draw_screener(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let q = &app.quant;
    let rows = ratatui::layout::Layout::vertical([Constraint::Length(1), Constraint::Min(5)])
        .split(area);

    let strategy = crate::api::STRATEGIES[q.screen_strategy];
    let busy = if q.screen_busy { "  …screening" } else { "" };
    frame.render_widget(
        Paragraph::new(format!(
            "strategy {}   a={}   b={}   rows {}{busy}   (←→ strategy, [ ] a, - = b, Enter refresh)",
            strategy,
            q.screen_a,
            q.screen_b,
            q.screen.len()
        ))
        .style(Style::default().fg(CYAN)),
        rows[0],
    );

    if q.screen.is_empty() {
        let msg = match &q.error {
            Some(e) => format!("quant API unreachable: {e}"),
            None => "no screener data — press Enter".to_string(),
        };
        frame.render_widget(placeholder(&msg), rows[1]);
        return;
    }

    let header = Row::new(["Symbol", "Price", "Ret 1M", "RSI14", "vs SMA50", "Signal"])
        .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
    let body = q.screen.iter().map(|r| {
        let sig_style = if r.signal == "long" {
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        Row::new(vec![
            Span::raw(r.symbol.clone()),
            Span::raw(r.price.map(fmt_n).unwrap_or_else(|| "--".into())),
            Span::raw(r.ret_1m.map(fmt_pct).unwrap_or_else(|| "--".into())),
            Span::raw(r.rsi14.map(|v| format!("{v:.1}")).unwrap_or_else(|| "--".into())),
            Span::raw(r.vs_sma50.map(fmt_pct).unwrap_or_else(|| "--".into())),
            Span::styled(r.signal.clone(), sig_style),
        ])
    });
    let table = Table::new(
        body,
        [
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(Block::bordered().title(" Screener "))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");
    let mut st =
        ratatui::widgets::TableState::default().with_selected(Some(q.screen_sel.min(q.screen.len() - 1)));
    frame.render_stateful_widget(table, rows[1], &mut st);
}

// --------------------------------------------------------------- backtest

const BT_FIELDS: [&str; 6] = ["symbol", "strategy", "a", "b", "cost_bps", "run ↵"];

fn draw_backtest(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let q = &app.quant;
    let cols = ratatui::layout::Layout::horizontal([Constraint::Length(42), Constraint::Min(40)])
        .split(area);

    // ---- form
    let symbol = q.symbols.get(q.bt_symbol).cloned().unwrap_or_else(|| "—".into());
    let values = [symbol,
        crate::api::STRATEGIES[q.bt_strategy].to_string(),
        q.bt_a.to_string(),
        q.bt_b.to_string(),
        format!("{:.1}", q.bt_cost_bps),
        if q.bt_busy { "running…".into() } else { "run".into() }];
    let lines: Vec<Line> = BT_FIELDS
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(i, (name, value))| {
            let focused = i == q.bt_field;
            let marker = if focused { ">" } else { " " };
            let style = if focused {
                Style::default().fg(Color::Black).bg(CYAN).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(
                format!(" {marker} {:<10} {:>16} ", name, value),
                style,
            ))
        })
        .collect();
    let form = Paragraph::new(lines).block(Block::bordered().title(" Backtest "));
    frame.render_widget(form, cols[0]);

    // ---- results
    let right = ratatui::layout::Layout::vertical([
        Constraint::Length(11),
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(cols[1]);

    match (&q.bt_result, &q.bt_error) {
        (None, Some(e)) => {
            frame.render_widget(placeholder(&format!("backtest failed: {e}")), right[0]);
            frame.render_widget(placeholder("no equity curve"), right[2]);
            frame.render_widget(placeholder("no position series"), right[3]);
        }
        (None, None) => {
            frame.render_widget(placeholder("set parameters, press Enter to run"), right[0]);
            frame.render_widget(placeholder("no equity curve"), right[2]);
            frame.render_widget(placeholder("no position series"), right[3]);
        }
        (Some(b), _) => {
            draw_metrics(frame, b, right[0]);
            draw_cost_line(frame, b, q.bt_cost_bps, right[1]);
            draw_equity(frame, b, right[2]);
            draw_position(frame, b, right[3]);
        }
    }
}

/// The "costs cost you X points" line — cost_drag_cagr in percentage points.
fn draw_cost_line(frame: &mut Frame<'_>, b: &Backtest, cost_bps: f64, area: Rect) {
    let pts = b.cost_drag_cagr * 100.0;
    let (text, color) = if pts > 0.0 {
        (
            format!("costs cost you {pts:.2} points of CAGR (cost {cost_bps:.1} bps)"),
            YELLOW,
        )
    } else {
        (
            format!("costs cost you ~0.00 points (cost {cost_bps:.1} bps)"),
            DIM,
        )
    };
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(color)), area);
}

/// Position series (0 = flat, 1 = long) as an in-market strip.
fn draw_position(frame: &mut Frame<'_>, b: &Backtest, area: Rect) {
    if b.position.is_empty() || area.width < 4 {
        frame.render_widget(placeholder("no position series"), area);
        return;
    }
    let buckets = (area.width.saturating_sub(2) as usize).max(1);
    let mut series = vec![0u64; buckets];
    for (i, p) in b.position.iter().enumerate() {
        if *p > 0.5 {
            let idx = (i * buckets / b.position.len()).min(buckets - 1);
            series[idx] = 1;
        }
    }
    let chart = ratatui::widgets::Sparkline::default()
        .block(Block::bordered().title(" position (1 = in market) "))
        .data(&series)
        .max(1)
        .style(Style::default().fg(GREEN));
    frame.render_widget(chart, area);
}

fn draw_metrics(frame: &mut Frame<'_>, b: &Backtest, area: Rect) {
    let header = Row::new(["metric", "strategy", "buy & hold"])
        .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
    let body = vec![
        Row::new(vec![
            "cagr".to_string(),
            fmt_pct(b.strategy.cagr),
            fmt_pct(b.buy_hold.cagr),
        ]),
        Row::new(vec![
            "sharpe".to_string(),
            fmt_n(b.strategy.sharpe),
            fmt_n(b.buy_hold.sharpe),
        ]),
        Row::new(vec![
            "max drawdown".to_string(),
            fmt_pct(b.strategy.max_drawdown),
            fmt_pct(b.buy_hold.max_drawdown),
        ]),
        Row::new(vec!["trades".to_string(), b.trades.to_string(), "—".into()]),
        Row::new(vec![
            "time in market".to_string(),
            fmt_pct(b.exposure),
            "—".into(),
        ]),
        Row::new(vec![
            "in market days".to_string(),
            if b.position.is_empty() {
                "—".to_string()
            } else {
                format!(
                    "{} / {}",
                    b.position.iter().filter(|p| **p > 0.5).count(),
                    b.position.len()
                )
            },
            "—".into(),
        ]),
        Row::new(vec!["bars".to_string(), b.dates.len().to_string(), "—".into()]),
    ];
    let table = Table::new(
        body,
        [Constraint::Length(18), Constraint::Length(16), Constraint::Length(16)],
    )
    .header(header)
    .block(Block::bordered().title(format!(" Results — {} ", b.symbol)));
    frame.render_widget(table, area);
}

fn draw_equity(frame: &mut Frame<'_>, b: &Backtest, area: Rect) {
    if b.equity.len() < 2 {
        frame.render_widget(placeholder("no equity curve"), area);
        return;
    }
    let strat: Vec<(f64, f64)> = b.equity.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect();
    let bh: Vec<(f64, f64)> = b
        .buy_hold_equity
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f64, *v))
        .collect();

    let lo = strat
        .iter()
        .chain(bh.iter())
        .map(|(_, v)| *v)
        .fold(f64::INFINITY, f64::min);
    let hi = strat
        .iter()
        .chain(bh.iter())
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max);
    let pad = ((hi - lo) * 0.05).max(0.01);
    let yb = [lo - pad, hi + pad];
    let xb = [0.0, (b.equity.len() - 1) as f64];

    let datasets = vec![
        Dataset::default()
            .name("strategy")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(GREEN))
            .data(&strat),
        Dataset::default()
            .name("buy & hold")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Blue))
            .data(&bh),
    ];

    let x_labels = [b.dates.first().cloned().unwrap_or_default(),
        b.dates.get(b.dates.len() / 2).cloned().unwrap_or_default(),
        b.dates.last().cloned().unwrap_or_default()];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title(" Equity (growth of 1) "))
        .x_axis(
            Axis::default()
                .bounds(xb)
                .labels(x_labels.iter().map(|l| Line::from(l.clone()))),
        )
        .y_axis(
            Axis::default()
                .bounds(yb)
                .labels([format!("{:.2}", yb[0]), format!("{:.2}", (yb[0] + yb[1]) / 2.0), format!("{:.2}", yb[1])]
                    .iter()
                    .map(|l| Line::from(l.clone()))),
        );
    frame.render_widget(chart, area);
}

// ----------------------------------------------------------------- market

fn draw_market(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let q = &app.quant;
    let cols = ratatui::layout::Layout::horizontal([Constraint::Length(38), Constraint::Min(40)])
        .split(area);

    let symbol = q.symbols.get(q.mk_symbol).cloned().unwrap_or_else(|| "—".into());
    let values = [symbol,
        format!("{} days", q.mk_days),
        if q.mk_busy { "loading…".into() } else { "fetch ↵".into() }];
    let names = ["symbol", "window", "action"];
    let lines: Vec<Line> = names
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(i, (name, value))| {
            let focused = i == q.mk_field;
            let style = if focused {
                Style::default().fg(Color::Black).bg(CYAN).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(
                format!(" {} {:<8} {:>14} ", if focused { ">" } else { " " }, name, value),
                style,
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Prices ")),
        cols[0],
    );

    let right = ratatui::layout::Layout::vertical([Constraint::Min(8), Constraint::Length(9)])
        .split(cols[1]);
    match q.mk_prices.as_ref() {
        None => {
            let msg = if q.symbols.is_empty() {
                "quant API unreachable — symbols not loaded".to_string()
            } else {
                "press Enter to load bars".to_string()
            };
            frame.render_widget(placeholder(&msg), right[0]);
            frame.render_widget(placeholder("no bars"), right[1]);
        }
        Some(p) => draw_price_chart(frame, p, right[0]),
    }
}

fn draw_price_chart(frame: &mut Frame<'_>, p: &Prices, area: Rect) {
    let cols = ratatui::layout::Layout::vertical([Constraint::Min(6), Constraint::Length(9)])
        .split(area);

    if p.bars.len() < 2 {
        frame.render_widget(placeholder("not enough bars"), cols[0]);
        return;
    }
    let closes: Vec<(f64, f64)> = p
        .bars
        .iter()
        .enumerate()
        .map(|(i, b)| (i as f64, b.c))
        .collect();
    let lo = closes.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
    let hi = closes.iter().map(|(_, v)| *v).fold(f64::NEG_INFINITY, f64::max);
    let pad = ((hi - lo) * 0.05).max(0.01);
    let last = p.bars.last().unwrap();

    let datasets = vec![Dataset::default()
        .name("close")
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(CYAN))
        .data(&closes)];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title(format!(
            " {} close — last {:.2} ({}) ",
            p.symbol, last.c, p.bars.first().unwrap().t
        )))
        .x_axis(
            Axis::default()
                .bounds([0.0, (p.bars.len() - 1) as f64])
                .labels([p.bars.first().unwrap().t.clone(), p.bars.last().unwrap().t.clone()]
                    .iter()
                    .map(|l| Line::from(l.clone()))),
        )
        .y_axis(
            Axis::default()
                .bounds([lo - pad, hi + pad])
                .labels([format!("{lo:.2}"), format!("{hi:.2}")].iter().map(|l| Line::from(l.clone()))),
        );
    frame.render_widget(chart, cols[0]);

    // recent bars
    let header = Row::new(["Date", "Open", "High", "Low", "Close"])
        .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
    let body = p
        .bars
        .iter()
        .rev()
        .take(7)
        .map(|b| {
            Row::new(vec![
                b.t.clone(),
                fmt_n(b.o),
                fmt_n(b.h),
                fmt_n(b.l),
                fmt_n(b.c),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        body,
        [
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .block(Block::bordered().title(" Last bars "));
    frame.render_widget(table, cols[1]);
}

// ---------------------------------------------------------------- helpers

fn placeholder(text: &str) -> Paragraph<'_> {
    Paragraph::new(text)
        .style(Style::default().fg(DIM))
        .alignment(Alignment::Center)
        .block(Block::bordered())
}

fn kv(key: &'static str, value: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{key:<10}")),
        Span::styled(value.into(), Style::default().fg(color)),
    ])
}

fn fmt_pct(v: f64) -> String {
    format!("{:+.2}%", v * 100.0)
}

fn fmt_n(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.4}")
    }
}

fn fmt_uptime(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
}

fn fmt_ts(ms: u64) -> String {
    match DateTime::from_timestamp_millis(ms as i64) {
        Some(dt) => dt.with_timezone(&Utc).format("%H:%M:%S").to_string(),
        None => ms.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Backtest, EngineClient, QuantClient, Stats};
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::mpsc;

    fn sample_backtest() -> Backtest {
        Backtest {
            symbol: "AAPL".into(),
            dates: vec!["2024-01-01".into(), "2024-01-02".into(), "2024-01-03".into()],
            strategy: Stats { cagr: 0.11, sharpe: 1.2, max_drawdown: -0.18 },
            buy_hold: Stats { cagr: 0.09, sharpe: 0.9, max_drawdown: -0.31 },
            trades: 42,
            exposure: 0.87,
            equity: vec![1.0, 1.01, 1.02],
            buy_hold_equity: vec![1.0, 0.99, 1.01],
            position: vec![0.0, 1.0, 1.0],
            cost_drag_cagr: 0.00515,
        }
    }

    fn app_with_result() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        let engine = EngineClient::new("http://127.0.0.1:1").unwrap();
        let quant = QuantClient::new("http://127.0.0.1:1").unwrap();
        let mut app = App::new(engine, quant, tx);
        app.tab = Tab::Backtest;
        app.quant.symbols = vec!["AAPL".into()];
        app.quant.bt_cost_bps = 10.0;
        app.quant.bt_result = Some(sample_backtest());
        app
    }

    fn rendered(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn backtest_tab_renders_metrics_cost_line_and_position() {
        let text = rendered(&app_with_result());
        assert!(text.contains("time in market"), "metrics row missing");
        assert!(text.contains("in market days"), "positions row missing");
        assert!(text.contains("costs cost you"), "cost line missing: {text}");
        assert!(text.contains("points of CAGR"), "cost line missing: {text}");
        assert!(text.contains("position (1 = in market)"), "position strip missing");
        assert!(text.contains("Equity"), "equity chart missing");
    }

    #[test]
    fn all_tabs_render_without_panic() {
        let mut app = app_with_result();
        app.quant.bt_result = None;
        for tab in [Tab::Engine, Tab::Screener, Tab::Backtest, Tab::Market] {
            app.tab = tab;
            let text = rendered(&app);
            assert!(text.contains("Aegis"), "header missing for {tab:?}");
            assert!(text.contains("Screener"), "tabs missing for {tab:?}");
        }
    }
}
