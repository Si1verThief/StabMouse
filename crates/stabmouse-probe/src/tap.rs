//! Does a virtual tablet's tip reach an ordinary application as a click?
//!
//! In tablet mode the pen button becomes `BTN_TOUCH` on a tablet device, not a button on a
//! mouse. Whether that turns into a click for an application depends entirely on the
//! compositor emulating a pointer for clients that do not speak the tablet protocol — and most
//! toolkits do not. If it does not emulate, tablet mode makes every non-drawing application
//! unusable, which is a far bigger problem than it first sounds.
//!
//! This taps at a chosen point with a tablet, and can tap the same point with a relative mouse
//! for comparison. The mouse arm is the control: if neither registers, the harness is wrong
//! rather than the tablet.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use stabmouse_output::{MouseSink, TabletSink, SURFACE_MAX};
use std::time::Duration;

#[derive(ClapArgs)]
pub struct Args {
    /// Horizontal position across the surface, 0.0 to 1.0.
    #[arg(long, default_value_t = 0.5)]
    x: f64,
    /// Vertical position across the surface, 0.0 to 1.0.
    #[arg(long, default_value_t = 0.5)]
    y: f64,
    /// Tap with a relative mouse instead, as a control.
    #[arg(long)]
    mouse: bool,
    /// Hover the tablet at the point, then click with a *mouse*.
    ///
    /// The decisive control. If this registers, the pointer really was over the target — so a
    /// tablet moves the pointer, and only the tip failing to become a click is at fault. If it
    /// does not, the aim was wrong and nothing has been learned about the tip.
    #[arg(long)]
    hover_then_click: bool,
    /// Hover the tablet at the point, then nudge a relative mouse.
    ///
    /// Answers whether the cursor continues from where the pen left it or jumps to wherever
    /// the pointer was last — which decides whether falling back from tablet to mouse output
    /// can be seamless.
    #[arg(long)]
    hover_then_move: bool,
    /// Keep a virtual mouse alive and nudging for this many seconds.
    ///
    /// Gives the daemon a source device to read that is not the user's real mouse, so its
    /// behaviour can be exercised without touching their session.
    #[arg(long, default_value_t = 0)]
    hold: u64,
    /// Seconds to wait before tapping, to allow a window to be focused.
    #[arg(long, default_value_t = 3)]
    delay: u64,
}

pub fn run(args: Args) -> Result<()> {
    std::thread::sleep(Duration::from_secs(args.delay));

    if args.hold > 0 {
        return hold_a_mouse(args.hold);
    }
    if args.mouse {
        return tap_with_mouse();
    }
    if args.hover_then_click {
        return hover_then_click(args.x, args.y);
    }
    if args.hover_then_move {
        return hover_then_move(args.x, args.y);
    }
    tap_with_tablet(args.x, args.y)
}

fn tap_with_tablet(fx: f64, fy: f64) -> Result<()> {
    let mut tablet = TabletSink::new("StabMouse tap probe").context("creating the tablet")?;

    let x = (fx.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;
    let y = (fy.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;

    // The compositor has to adopt the device before anything sent on it means anything.
    std::thread::sleep(Duration::from_millis(300));

    // Hover first. A tool that appears already touching is an odd state, and hovering is also
    // what moves the emulated pointer into position before the tip lands.
    tablet.pen(x, y, 0.0, false);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(150));

    println!("tablet: tip down at ({x}, {y})");
    tablet.pen(x, y, 0.8, true);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(120));

    tablet.pen(x, y, 0.0, false);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(150));

    tablet.leave_proximity();
    tablet.flush()?;
    println!("tablet: done");
    Ok(())
}

fn hover_then_click(fx: f64, fy: f64) -> Result<()> {
    let mut tablet = TabletSink::new("StabMouse tap probe").context("creating the tablet")?;
    let x = (fx.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;
    let y = (fy.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;

    std::thread::sleep(Duration::from_millis(300));
    println!("tablet: hovering at ({x}, {y}) without touching");
    tablet.pen(x, y, 0.0, false);
    tablet.flush()?;
    // Nudged, because a position identical to the last one is suppressed and would move
    // nothing at all.
    std::thread::sleep(Duration::from_millis(100));
    tablet.pen(x + 1, y + 1, 0.0, false);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(400));

    println!("mouse: clicking wherever the tablet left the pointer");
    tap_with_mouse()?;

    std::thread::sleep(Duration::from_millis(200));
    tablet.leave_proximity();
    tablet.flush()?;
    Ok(())
}

fn hover_then_move(fx: f64, fy: f64) -> Result<()> {
    let mut tablet = TabletSink::new("StabMouse tap probe").context("creating the tablet")?;
    let x = (fx.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;
    let y = (fy.clamp(0.0, 1.0) * f64::from(SURFACE_MAX)) as i32;

    std::thread::sleep(Duration::from_millis(300));
    println!("PHASE1: tablet hovering at ({x}, {y})");
    tablet.pen(x, y, 0.0, false);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(120));
    tablet.pen(x + 1, y + 1, 0.0, false);
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(2500));

    // Leave proximity first, exactly as a fallback to mouse output would.
    tablet.leave_proximity();
    tablet.flush()?;
    std::thread::sleep(Duration::from_millis(200));

    let mut keys = evdev::AttributeSet::<evdev::KeyCode>::new();
    keys.insert(evdev::KeyCode::BTN_LEFT);
    let mut axes = evdev::AttributeSet::<evdev::RelativeAxisCode>::new();
    axes.insert(evdev::RelativeAxisCode::REL_X);
    axes.insert(evdev::RelativeAxisCode::REL_Y);
    let mut mouse = MouseSink::new("StabMouse continuity probe", &keys, &axes)
        .context("creating the mouse")?;
    std::thread::sleep(Duration::from_millis(300));

    println!("PHASE2: nudging the relative pointer by +40,0");
    mouse.motion(40, 0);
    mouse.flush()?;
    std::thread::sleep(Duration::from_millis(2500));
    println!("done");
    Ok(())
}

/// A virtual mouse that stays alive and moves gently, as a stand-in source device.
fn hold_a_mouse(seconds: u64) -> Result<()> {
    let mut keys = evdev::AttributeSet::<evdev::KeyCode>::new();
    keys.insert(evdev::KeyCode::BTN_LEFT);
    keys.insert(evdev::KeyCode::BTN_RIGHT);
    let mut axes = evdev::AttributeSet::<evdev::RelativeAxisCode>::new();
    axes.insert(evdev::RelativeAxisCode::REL_X);
    axes.insert(evdev::RelativeAxisCode::REL_Y);
    axes.insert(evdev::RelativeAxisCode::REL_WHEEL);

    let mut mouse = MouseSink::new("StabMouse source stand-in", &keys, &axes)
        .context("creating the stand-in mouse")?;
    for node in mouse.nodes() {
        println!("source node: {}", node.display());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    let mut sign = 1;
    while std::time::Instant::now() < deadline {
        mouse.motion(sign, 0);
        mouse.flush()?;
        sign = -sign;
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(())
}

fn tap_with_mouse() -> Result<()> {
    let mut keys = evdev::AttributeSet::<evdev::KeyCode>::new();
    keys.insert(evdev::KeyCode::BTN_LEFT);
    let mut axes = evdev::AttributeSet::<evdev::RelativeAxisCode>::new();
    axes.insert(evdev::RelativeAxisCode::REL_X);
    axes.insert(evdev::RelativeAxisCode::REL_Y);

    let mut mouse = MouseSink::new("StabMouse tap probe mouse", &keys, &axes)
        .context("creating the mouse")?;
    std::thread::sleep(Duration::from_millis(300));

    println!("mouse: click where the pointer already is");
    mouse.key(evdev::KeyCode::BTN_LEFT.code(), true);
    mouse.flush()?;
    std::thread::sleep(Duration::from_millis(80));
    mouse.key(evdev::KeyCode::BTN_LEFT.code(), false);
    mouse.flush()?;
    println!("mouse: done");
    Ok(())
}
