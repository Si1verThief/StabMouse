//! `stabmouse` — the command-line client.
//!
//! Separate from `stabmoused` on purpose. The daemon holds an `EVIOCGRAB` on the user's mouse;
//! a client that talks to it should not be the same binary that could be started by accident
//! and take the pointer. It also matches D4: the daemon is headless and every frontend is a
//! separate process speaking D-Bus.
//!
//! # Exit codes carry meaning
//!
//! docs/api.md fixes them, and one is load-bearing: **4 means no daemon**, distinct from a
//! generic error, so a script can start one and retry rather than giving up. **3 means a
//! permission problem**, which is the most likely first-run failure — not being in the `input`
//! group — and deserves to be scriptable rather than folded into "something went wrong".

use clap::{Parser, Subcommand};
use stabmouse_ipc::{client::Client, Error};

#[derive(Parser)]
#[command(
    name = "stabmouse",
    about = "Control a running StabMouse daemon",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress success output. Errors are still reported.
    #[arg(long, short, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// What the daemon is doing.
    Status,
    /// Advance to the next mode. Bind this to a hotkey.
    Switch,
    /// Jump to a mode by slot number or name.
    Mode {
        /// A 1-based slot, or a mode name such as `draw`.
        which: String,
    },
    /// List the mode slots in the active profile.
    Modes,
    /// Release the grab and stop filtering.
    #[command(alias = "release")]
    Panic,
    /// Resume filtering after a panic.
    Resume,
    /// Enable or disable filtering.
    Enable {
        /// Turn it off instead of on.
        #[arg(long)]
        off: bool,
    },
    /// Re-read the config now, without waiting for the file poll.
    Reload,
    /// Ask the daemon to exit.
    Quit,
    /// Input devices and whether they are managed.
    Devices,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            // An absent daemon is the one failure with an obvious next step, so it gets said.
            if matches!(e, Error::NoDaemon) {
                eprintln!("Start one with: stabmoused run");
            }
            std::process::ExitCode::from(e.exit_code().code() as u8)
        }
    }
}

fn run(cli: &Cli) -> stabmouse_ipc::Result<()> {
    let client = Client::connect()?;
    let say = |s: &str| {
        if !cli.quiet {
            println!("{s}");
        }
    };

    match &cli.command {
        Command::Status => {
            let status = client.status()?;
            if cli.json {
                print_json(&status);
            } else {
                print_status(&status);
            }
        }
        Command::Switch => {
            let slot = client.toggle_mode()?;
            say(&format!("mode {slot}"));
        }
        Command::Mode { which } => {
            // A bare number is a slot; anything else is a name. Trying the number first means
            // a mode literally named "2" is unreachable by name, which is a fair trade for
            // `stabmouse mode 2` doing the obvious thing.
            match which.parse::<u32>() {
                Ok(slot) => client.set_mode(slot)?,
                Err(_) => client.set_mode_by_name(which)?,
            }
            say(&format!("mode {which}"));
        }
        Command::Modes => {
            let modes = client.modes()?;
            if cli.json {
                for m in &modes {
                    println!(
                        r#"{{"slot":{},"name":"{}","output":"{}","preset":"{}"}}"#,
                        m.slot, m.name, m.output, m.preset
                    );
                }
            } else {
                for m in &modes {
                    println!("{}: {} — {} via '{}'", m.slot, m.name, m.output, m.preset);
                }
            }
        }
        Command::Panic => {
            client.panic()?;
            say("grab released; `stabmouse resume` to continue");
        }
        Command::Resume => {
            client.resume()?;
            say("filtering again");
        }
        Command::Enable { off } => {
            client.set_enabled(!off)?;
            say(if *off { "disabled" } else { "enabled" });
        }
        Command::Reload => {
            client.reload()?;
            say("config reloaded");
        }
        Command::Quit => {
            client.quit()?;
            say("stopping");
        }
        Command::Devices => {
            let devices = client.devices()?;
            if devices.is_empty() {
                say("the daemon reports no devices");
            }
            for d in &devices {
                println!(
                    "{:<24} {:<40} {}",
                    d.id,
                    d.name,
                    if d.managed { "managed" } else { "ignored" }
                );
            }
        }
    }
    Ok(())
}

fn get<'a>(
    map: &'a std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<&'a zbus::zvariant::OwnedValue> {
    map.get(key)
}

fn as_string(v: Option<&zbus::zvariant::OwnedValue>) -> String {
    v.map(|v| {
        // The wire carries variants; rendering falls back to the debug form for types this
        // build does not specifically know, so a daemon that grows a field stays readable
        // rather than showing nothing.
        String::try_from(v.clone())
            .or_else(|_| bool::try_from(v.clone()).map(|b| b.to_string()))
            .or_else(|_| u32::try_from(v.clone()).map(|n| n.to_string()))
            .unwrap_or_else(|_| format!("{v:?}"))
    })
    .unwrap_or_else(|| "—".into())
}

fn print_status(map: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>) {
    println!(
        "mode {} — {}",
        as_string(get(map, "mode_slot")),
        as_string(get(map, "mode_name"))
    );
    println!("profile:  {}", as_string(get(map, "profile")));
    println!(
        "state:    {}",
        if as_string(get(map, "panicked")) == "true" {
            "PANICKED — the grab is released"
        } else {
            "active"
        }
    );
    println!(
        "tablets:  {} ({})",
        as_string(get(map, "tablets")),
        if as_string(get(map, "tablets_placed")) == "true" {
            "confined to their screens"
        } else {
            "not confined"
        }
    );
    if as_string(get(map, "degraded")) == "true" {
        // Degradation is the state most likely to be mistaken for the feature being broken, so
        // it is spelled out rather than shown as a flag.
        println!("degraded: {}", as_string(get(map, "degraded_reason")));
    }
    println!("version:  {}", as_string(get(map, "version")));
}

/// One value, rendered with its real JSON type.
///
/// Numbers and booleans are emitted unquoted. `--json` exists to be parsed, and a consumer
/// that has to strip quotes and re-parse `"2"` into a number is being handed a string that
/// merely looks structured.
fn as_json(v: Option<&zbus::zvariant::OwnedValue>) -> String {
    let Some(v) = v else { return "null".into() };
    if let Ok(b) = bool::try_from(v.clone()) {
        return b.to_string();
    }
    if let Ok(n) = u32::try_from(v.clone()) {
        return n.to_string();
    }
    if let Ok(s) = String::try_from(v.clone()) {
        return format!("\"{}\"", escape(&s));
    }
    format!("\"{}\"", escape(&format!("{v:?}")))
}

fn print_json(map: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>) {
    // Hand-written rather than pulling in a JSON crate for one output path. Keys are fixed
    // identifiers from the daemon and strings are escaped, so this cannot produce broken JSON.
    let mut keys: Vec<&String> = map.keys().collect();
    // Sorted so the output is stable between runs, which matters for diffing and for tests.
    keys.sort();
    let body: Vec<String> = keys
        .iter()
        .map(|k| format!("\"{}\":{}", k, as_json(map.get(*k))))
        .collect();
    println!("{{{}}}", body.join(","));
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_survives_quotes_and_backslashes() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_values_keep_their_type() {
        use zbus::zvariant::{OwnedValue, Value};
        let num = OwnedValue::try_from(Value::from(2u32)).unwrap();
        let yes = OwnedValue::try_from(Value::from(true)).unwrap();
        let text = OwnedValue::try_from(Value::from("Draw")).unwrap();

        // Unquoted, so a consumer does not have to strip quotes and re-parse.
        assert_eq!(as_json(Some(&num)), "2");
        assert_eq!(as_json(Some(&yes)), "true");
        assert_eq!(as_json(Some(&text)), r#""Draw""#);
        assert_eq!(as_json(None), "null");
    }

    #[test]
    fn a_numeric_argument_is_a_slot_and_a_word_is_a_name() {
        assert_eq!("2".parse::<u32>().ok(), Some(2));
        assert!("draw".parse::<u32>().is_err());
    }

    #[test]
    fn exit_codes_reach_the_process() {
        // The contract scripts depend on: absent daemon is 4, not 1.
        assert_eq!(Error::NoDaemon.exit_code(), stabmouse_ipc::ExitCode::NoDaemon);
        assert_eq!(Error::NoDaemon.exit_code().code(), 4);
    }
}
