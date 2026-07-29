//! Platform capability probes.
//!
//! These exist to answer the open questions recorded in docs/modules.md before
//! the real crates are written — chiefly whether a uinput virtual tablet is
//! classified as a tablet by libinput, exposed via `tablet_v2` by KWin, and
//! delivered as pressure to applications that support it and as plain pointer
//! motion to those that do not.

mod latency;
mod tablet;

use anyhow::Result;
use clap::{Parser, Subcommand};
use evdev::{AbsoluteAxisCode, KeyCode, RelativeAxisCode};

#[derive(Parser)]
#[command(name = "stabmouse-probe", about = "Platform capability probes for StabMouse")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List evdev devices, so a source can be named explicitly.
    List,
    /// Create a virtual tablet, optionally driven from a real mouse.
    Tablet(tablet::Args),
    /// Measure evdev->uinput round-trip latency. Fully synthetic; safe to run.
    Latency(latency::Args),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::List => list(),
        Command::Tablet(args) => tablet::run(args),
        Command::Latency(args) => latency::run(args),
    }
}

/// Nothing is ever grabbed implicitly — the user picks a source from this list.
fn list() -> Result<()> {
    println!("{:<24} {:<44} {}", "PATH", "NAME", "LOOKS LIKE");

    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or("<unnamed>").to_owned();

        let has_rel = device
            .supported_relative_axes()
            .is_some_and(|a| a.contains(RelativeAxisCode::REL_X));
        let has_abs = device
            .supported_absolute_axes()
            .is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_X));
        let has_pen = device
            .supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_TOOL_PEN));
        let has_click = device
            .supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_LEFT));

        let kind = match (has_pen, has_abs, has_rel, has_click) {
            (true, _, _, _) => "tablet (pen)",
            (_, true, _, true) => "absolute pointer",
            (_, _, true, true) => "mouse",
            (_, _, true, false) => "relative (no buttons)",
            _ => "",
        };

        println!("{:<24} {:<44} {}", path.display(), truncate(&name, 44), kind);
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
