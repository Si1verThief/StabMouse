//! Can focus changes be tracked well enough to pick an output per application?
//!
//! Deciding tablet-versus-mouse output from the focused application only works if we learn
//! about a focus change **quickly and reliably**. This measures both.
//!
//! # Why a compositor script and not a protocol
//!
//! Checked against this session: of 67 advertised Wayland globals, none of
//! `org_kde_plasma_window_management`, `zwlr_foreign_toplevel_manager_v1`,
//! `ext_foreign_toplevel_list_v1` or `zcosmic_toplevel_info_v1` is offered to an ordinary
//! client. KWin keeps window management for privileged clients, so there is no protocol route
//! and no portable one either.
//!
//! That makes focus tracking the first genuinely KDE-shaped dependency in the project — the
//! tablet mapping in `stabmouse-desktop` at least had a portable half. Worth knowing before
//! building on it, which is what this probe is for.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// How long to watch for focus changes.
    #[arg(long, default_value_t = 30)]
    seconds: u64,
}

const SCRIPT_NAME: &str = "stabmouse-focus-spike";

/// The KWin script, matching what the daemon installs.
///
/// Reports the window **under the pointer**, because that is the surface a pen is delivered
/// to. Reporting the focused window instead was the original bug: KDE is click-to-focus, so
/// hovering over an application that cannot accept a pen changed nothing until the next click.
const SCRIPT: &str = r#"
var last = null;

function evaluate() {
    var found = workspace.windowAt(workspace.cursorPos, 1);
    var cls = (found && found.length > 0 && found[0].resourceClass)
        ? String(found[0].resourceClass) : "";
    var cap = (found && found.length > 0 && found[0].pid !== undefined)
        ? ("pid=" + String(found[0].pid)) : "no-pid";
    if (cls === last) {
        return;
    }
    last = cls;
    callDBus("io.github.si1verthief.StabMouse.FocusSpike",
             "/focus", "io.github.si1verthief.StabMouse.FocusSpike",
             "Activated", cls, cap);
}

workspace.cursorPosChanged.connect(evaluate);
workspace.windowActivated.connect(evaluate);
"#;

struct Spike {
    started: Instant,
    seen: std::sync::Arc<std::sync::Mutex<Vec<(Duration, String)>>>,
}

#[zbus::interface(name = "io.github.si1verthief.StabMouse.FocusSpike")]
impl Spike {
    fn activated(&self, class: String, caption: String) {
        let at = self.started.elapsed();
        let label = if class.is_empty() {
            "(none)".to_string()
        } else {
            class
        };
        println!(
            "  {:>7.3}s  {:<28} {}",
            at.as_secs_f64(),
            label,
            caption.chars().take(40).collect::<String>()
        );
        let _ = std::io::stdout().flush();
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((at, label));
        }
    }
}

fn kwin(method: &str, arg: &str) -> Result<String> {
    let out = Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
            method,
            "s",
            arg,
        ])
        .output()
        .context("calling KWin's scripting interface")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn run(args: Args) -> Result<()> {
    let path = std::env::temp_dir().join(format!("{SCRIPT_NAME}.js"));
    std::fs::write(&path, SCRIPT).context("writing the KWin script")?;

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let spike = Spike {
        started: Instant::now(),
        seen: seen.clone(),
    };

    // The service has to exist before the script can call it, or the first activation is lost.
    let _conn = zbus::blocking::connection::Builder::session()?
        .name("io.github.si1verthief.StabMouse.FocusSpike")?
        .serve_at("/focus", spike)?
        .build()
        .context("claiming the spike's bus name")?;

    // A previous run's script would otherwise still be attached and double-report.
    let _ = kwin("unloadScript", SCRIPT_NAME);
    // Named explicitly, so `unloadScript` by the same name actually removes it. Loading
    // without a name lets KWin choose one, and the unload then silently matches nothing.
    let loaded = String::from_utf8_lossy(
        &Command::new("busctl")
            .args([
                "--user", "call", "org.kde.KWin", "/Scripting",
                "org.kde.kwin.Scripting", "loadScript", "ss",
                path.to_str().unwrap_or_default(), SCRIPT_NAME,
            ])
            .output()
            .context("loading the KWin script")?
            .stdout,
    )
    .trim()
    .to_string();
    println!("loaded KWin script ({loaded})");

    Command::new("busctl")
        .args([
            "--user", "call", "org.kde.KWin", "/Scripting",
            "org.kde.kwin.Scripting", "start",
        ])
        .output()
        .ok();

    println!(
        "\nwatching for {}s — move the pointer over each application in turn.\n\
         These are the exact strings to use in config's [tablet_support].",
        args.seconds
    );
    println!("  {:>7}  {:<28} {}", "at", "class", "title");
    std::thread::sleep(Duration::from_secs(args.seconds));

    // Always removed. Leaving a script attached to the user's compositor after a diagnostic
    // has finished is not acceptable, and it would double-report on the next run.
    let _ = kwin("unloadScript", SCRIPT_NAME);
    let _ = std::fs::remove_file(&path);

    let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
    println!("\nunloaded the script");
    println!("{} focus changes seen", seen.len());
    if seen.is_empty() {
        println!(
            "Nothing arrived. Either no window was activated, or the script could not reach\n\
             the bus — which would rule this approach out."
        );
    }
    Ok(())
}
