//! Aegis TUI — terminal front-end for both REST APIs:
//! * Rust control-plane api-server (`/api/v1/...`)
//! * Python quant API in `restapis/` (`/health`, `/symbols`, `/prices`, `/backtest`, `/screen`)

mod api;
mod app;
mod ui;

use std::io::{self, Stdout};
use std::thread;

use anyhow::Context;
use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::Terminal;
use tokio::sync::mpsc;

use api::{EngineClient, QuantClient};
use app::{spawn_pollers, App, Msg};

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[derive(Parser)]
#[command(
    name = "aegis-tui",
    about = "Terminal UI for the Aegis control plane and quant REST APIs"
)]
struct Cli {
    /// Base URL of the Rust control-plane api-server.
    #[arg(long, env = "LQ_ENGINE_URL", default_value = "http://127.0.0.1:8080")]
    engine: String,

    /// Base URL of the quant (restapis) FastAPI service.
    #[arg(long, env = "LQ_QUANT_URL", default_value = "http://127.0.0.1:8000")]
    quant: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let engine = EngineClient::new(&cli.engine).context("engine client")?;
    let quant = QuantClient::new(&cli.quant).context("quant client")?;

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    spawn_pollers(engine.clone(), quant.clone(), msg_tx.clone());
    let mut app = App::new(engine, quant, msg_tx);

    // Input on its own thread: crossterm::event::read blocks, and the UI loop
    // owns the async runtime.
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Event>();
    thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut terminal = start_terminal().context("terminal setup")?;
    let result = run(&mut terminal, &mut app, &mut key_rx, &mut msg_rx).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run(
    terminal: &mut Tui,
    app: &mut App,
    key_rx: &mut mpsc::UnboundedReceiver<Event>,
    msg_rx: &mut mpsc::UnboundedReceiver<Msg>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        tokio::select! {
            event = key_rx.recv() => match event {
                Some(Event::Key(key)) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(_) => {}
                None => break,
            },
            msg = msg_rx.recv() => match msg {
                Some(msg) => app.update(msg),
                None => break,
            },
        }

        if app.quit {
            break;
        }
    }
    Ok(())
}

fn start_terminal() -> io::Result<Tui> {
    // Leave the terminal usable even if the UI panics.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
