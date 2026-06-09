//! CLI entry point for the Rust rewrite of armoury-tui.
//!
//! Three modes mirror the Python original:
//!
//!   (default)   launch the full ratatui dashboard
//!   --probe     print the discovered HardwareMap and exit (no TUI)
//!   --once      print one plain-text telemetry snapshot and exit
//!
//! Phase 0 wires the CLI, the tokio runtime and a minimal ratatui loop.
//! `--probe` / `--once` are stubbed until Phase A lands the data layer.

mod app;
mod config;
mod control;
mod dbus;
mod render;
mod scanner;
mod sysfs;
mod telemetry;
mod theme;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "armoury-tui",
    version,
    about = "ASUS Armoury-Crate-style monitor/control TUI for Linux (Rust)."
)]
struct Cli {
    /// Print discovered hardware map and exit.
    #[arg(long, conflicts_with = "once")]
    probe: bool,

    /// Print one telemetry snapshot and exit.
    #[arg(long)]
    once: bool,

    /// Print one telemetry snapshot as JSON and exit (for status bars/scripts).
    #[arg(long)]
    json: bool,

    /// UI refresh interval in seconds (default 1.0).
    #[arg(short = 'i', long, default_value_t = 1.0, value_name = "SEC")]
    interval: f64,

    /// Append a telemetry CSV row each tick to this file.
    #[arg(long, value_name = "CSV")]
    log: Option<String>,

    /// Summarise a --log CSV (stats + sparklines) and exit.
    #[arg(long, value_name = "CSV")]
    replay: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.probe {
        return scanner::probe();
    }
    if cli.once {
        return telemetry::once();
    }
    if cli.json {
        return telemetry::json();
    }
    if let Some(csv) = cli.replay {
        return telemetry::replay(&csv);
    }

    // Clamp like the Python version so a tiny interval can't spin the loop.
    let refresh = cli.interval.max(0.25);
    app::run(refresh, cli.log).await
}
