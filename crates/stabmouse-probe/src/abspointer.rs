//! Can an absolute *pointer* — not a tablet — drive the cursor? (P6)
//!
//! The fallback transport's open problem is that the compositor keeps the relative pointer's
//! position separately from the tablet's, so every transport change teleports the cursor to
//! wherever the pointer was last left. A VMware/QEMU-style absolute mouse — `ABS_X`/`ABS_Y`
//! plus mouse buttons, no pen or touch bits — would remove the divergence by construction:
//! every emit states the position outright, so there is nothing to drift.
//!
//! Three questions, answered in one closed loop with no human observation needed:
//!
//! 1. Does KWin/libinput accept such a device and move the visible cursor with it?
//! 2. What rectangle does the absolute range map onto — one screen, or the desktop's
//!    bounding box? Measured, not assumed, by emitting known fractions of the range and
//!    reading back `workspace.cursorPos`.
//! 3. Does `workspace.cursorPos` track a virtual *tablet* at all? The daemon's focus script
//!    hangs off `cursorPosChanged`; in use it appears blind while the pen drives, and this
//!    measures that directly.
//!
//! The read-back is a KWin script reporting `cursorPosChanged` over D-Bus — the same shape as
//! the focus probe, reporting positions instead of windows.
//!
//! **Motion only.** The device declares buttons so udev classifies it as a mouse, but the
//! probe never presses one; the worst it can do is move the cursor for a couple of seconds.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent,
    KeyCode, RelativeAxisCode, UinputAbsSetup,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// Milliseconds to wait for the compositor to adopt each device.
    #[arg(long, default_value_t = 1000)]
    settle_ms: u64,
}

const SCRIPT_NAME: &str = "stabmouse-abs-spike";
const ABS_MAX: i32 = 65535;

/// Reports every pointer-cursor move, plus one baseline report on load.
const SCRIPT: &str = r#"
function report() {
    var p = workspace.cursorPos;
    callDBus("io.github.si1verthief.StabMouse.AbsSpike",
             "/abs", "io.github.si1verthief.StabMouse.AbsSpike",
             "Moved", String(p.x), String(p.y));
}
workspace.cursorPosChanged.connect(report);
report();
"#;

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<(Instant, f64, f64)>>>);

struct Spike {
    log: Log,
}

#[zbus::interface(name = "io.github.si1verthief.StabMouse.AbsSpike")]
impl Spike {
    fn moved(&self, x: String, y: String) {
        let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>()) else {
            // If QPoint stops exposing .x/.y to scripts this prints the evidence.
            println!("  unparsable cursorPos report: ({x:?}, {y:?})");
            return;
        };
        if let Ok(mut log) = self.log.0.lock() {
            log.push((Instant::now(), x, y));
        }
    }
}

fn kwin_scripting(method: &str, args: &[&str]) -> Result<String> {
    let sig = "s".repeat(args.len());
    let mut cmd = Command::new("busctl");
    cmd.args(["--user", "call", "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", method]);
    if !args.is_empty() {
        cmd.arg(&sig);
        cmd.args(args);
    }
    let out = cmd.output().context("calling KWin's scripting interface")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build_abs_pointer() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_LEFT);
    keys.insert(KeyCode::BTN_RIGHT);
    keys.insert(KeyCode::BTN_MIDDLE);

    let mut wheel = AttributeSet::<RelativeAxisCode>::new();
    wheel.insert(RelativeAxisCode::REL_WHEEL);

    // Resolution 0: this is not a surface with a physical size, and giving it one invites
    // libinput to treat it as something it is not. VMware's virtual mouse ships the same way.
    let x = UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0));
    let y = UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0));

    Ok(VirtualDevice::builder()?
        .name("StabMouse probe abspointer")
        .with_keys(&keys)?
        .with_relative_axes(&wheel)?
        .with_absolute_axis(&x)?
        .with_absolute_axis(&y)?
        .build()?)
}

fn build_tablet() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_TOOL_PEN);
    keys.insert(KeyCode::BTN_TOUCH);

    let x = UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100));
    let y = UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100));
    let p = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_PRESSURE,
        AbsInfo::new(0, 0, 4095, 0, 0, 0),
    );

    Ok(VirtualDevice::builder()?
        .name("StabMouse probe abs-spike tablet")
        .with_keys(&keys)?
        .with_absolute_axis(&x)?
        .with_absolute_axis(&y)?
        .with_absolute_axis(&p)?
        .build()?)
}

fn abs_xy(device: &mut VirtualDevice, fx: f64, fy: f64) -> Result<()> {
    let x = (fx * f64::from(ABS_MAX)) as i32;
    let y = (fy * f64::from(ABS_MAX)) as i32;
    device.emit(&[
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
    ])?;
    Ok(())
}

/// The last report to arrive, if any.
fn latest(log: &Log) -> Option<(f64, f64)> {
    log.0.lock().ok()?.last().map(|&(_, x, y)| (x, y))
}

fn count(log: &Log) -> usize {
    log.0.lock().map(|l| l.len()).unwrap_or(0)
}

pub fn run(args: Args) -> Result<()> {
    let settle = Duration::from_millis(args.settle_ms);
    let log = Log::default();

    let _conn = zbus::blocking::connection::Builder::session()?
        .name("io.github.si1verthief.StabMouse.AbsSpike")?
        .serve_at("/abs", Spike { log: log.clone() })?
        .build()
        .context("claiming the spike's bus name")?;

    let path = std::env::temp_dir().join(format!("{SCRIPT_NAME}.js"));
    std::fs::write(&path, SCRIPT).context("writing the KWin script")?;
    let _ = kwin_scripting("unloadScript", &[SCRIPT_NAME]);
    let loaded = kwin_scripting(
        "loadScript",
        &[path.to_str().unwrap_or_default(), SCRIPT_NAME],
    )?;
    println!("loaded KWin script ({loaded})");
    kwin_scripting("start", &[])?;
    std::thread::sleep(Duration::from_millis(300));

    let baseline = latest(&log);
    match baseline {
        Some((x, y)) => println!("baseline cursorPos: ({x:.0}, {y:.0})"),
        None => println!(
            "no baseline report — the script could not reach the bus, results will show it"
        ),
    }

    // ---- Phase 1: the absolute pointer ------------------------------------------------
    println!("\n[1] absolute pointer: create, settle {}ms, emit known fractions", args.settle_ms);
    let mut pointer = build_abs_pointer().context("creating the absolute pointer")?;
    std::thread::sleep(settle);

    // Known fractions of the range. Off-centre and asymmetric, so a mapping to one screen
    // versus the desktop's bounding box produces visibly different pixels.
    let targets = [(0.25, 0.25), (0.75, 0.25), (0.50, 0.75)];
    let mut seen: Vec<Option<(f64, f64)>> = Vec::new();
    for &(fx, fy) in &targets {
        let before = count(&log);
        abs_xy(&mut pointer, fx, fy)?;
        std::thread::sleep(Duration::from_millis(250));
        let reported = if count(&log) > before { latest(&log) } else { None };
        match reported {
            Some((x, y)) => println!("  emitted ({fx:.2}, {fy:.2}) -> cursorPos ({x:.0}, {y:.0})"),
            None => println!("  emitted ({fx:.2}, {fy:.2}) -> no cursor movement reported"),
        }
        seen.push(reported);
    }

    let pointer_works = seen.iter().filter(|s| s.is_some()).count() >= 2;
    if let (Some((x0, y0)), Some((x1, y1)), (fx0, fy0), (fx1, fy1)) =
        (seen[0], seen[2], targets[0], targets[2])
    {
        // Two distinct fractions per axis give the implied linear map: origin + f * extent.
        let ex = (x1 - x0) / (fx1 - fx0);
        let ey = (y1 - y0) / (fy1 - fy0);
        println!(
            "  implied mapping: origin ({:.0}, {:.0}), extent {:.0} x {:.0}",
            x0 - fx0 * ex,
            y0 - fy0 * ey,
            ex,
            ey
        );
    }

    // ---- Phase 2: does cursorPos see a tablet at all? ---------------------------------
    println!("\n[2] virtual tablet: hover-move across the same fractions");
    let mut tablet = build_tablet().context("creating the spike tablet")?;
    std::thread::sleep(settle);

    let before_tablet = count(&log);
    tablet.emit(&[
        InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOOL_PEN.code(), 1),
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, ABS_MAX / 4),
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, ABS_MAX / 4),
    ])?;
    for &(fx, fy) in &targets {
        abs_xy(&mut tablet, fx, fy)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    tablet.emit(&[InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOOL_PEN.code(), 0)])?;
    std::thread::sleep(Duration::from_millis(250));
    let tablet_reports = count(&log) - before_tablet;

    let _ = kwin_scripting("unloadScript", &[SCRIPT_NAME]);
    let _ = std::fs::remove_file(&path);
    println!("\nunloaded the script");

    // ---- Verdicts ---------------------------------------------------------------------
    println!("\nverdicts:");
    println!(
        "  absolute pointer moves the cursor:        {}",
        if pointer_works { "YES" } else { "NO — rules the fallback design out" }
    );
    println!(
        "  cursorPos tracked the tablet:             {}",
        match tablet_reports {
            0 => "NO — confirms the focus script is blind while the pen drives".to_string(),
            n => format!("YES — {n} reports; the blindness theory is wrong"),
        }
    );
    Ok(())
}
