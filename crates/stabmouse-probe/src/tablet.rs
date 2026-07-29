//! Virtual tablet probe.
//!
//! Creates a uinput absolute-pointer device declaring `BTN_TOOL_PEN`, `BTN_TOUCH`
//! and `ABS_PRESSURE`, then optionally drives it from a real mouse so the tablet
//! can actually be drawn with.
//!
//! Safety: the source is only grabbed when `--grab` is passed. Without it you get
//! doubled cursor motion — visually messy, but it cannot leave you without a
//! pointer. The grab is held by a file descriptor, so process death of any kind
//! (including Ctrl-C) releases it via the kernel.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AbsoluteAxisCode, AttributeSet, Device, EventType, InputEvent, KeyCode,
    RelativeAxisCode, UinputAbsSetup,
};
use std::path::PathBuf;
use std::time::Instant;

/// Logical extent of the tablet surface. The compositor maps this onto the
/// screen, so the magnitude only sets positional resolution.
const ABS_MAX: i32 = 32767;

/// Pressure range, matching docs/stages.md.
const PRESSURE_MAX: i32 = 4095;

/// Probe-grade pressure shaping. The real values are Batch 8 territory; these
/// only need to be good enough to see pressure varying in an application.
const ATTACK_MS: f64 = 60.0;
const V_MAX_MM_S: f64 = 400.0;
const MIN_PRESSURE: f64 = 0.05;

#[derive(ClapArgs)]
pub struct Args {
    /// Source mouse, e.g. /dev/input/event2. Omit to create the tablet only,
    /// which is enough to check how libinput and KWin classify it.
    #[arg(long)]
    source: Option<PathBuf>,

    /// Take an exclusive grab on the source so the physical mouse stops driving
    /// the cursor directly. Off by default.
    #[arg(long)]
    grab: bool,

    /// Counts-to-tablet-units scale. Higher covers the screen with less hand
    /// movement.
    #[arg(long, default_value_t = 2.0)]
    scale: f64,

    /// Assumed source DPI, used only to convert counts to mm for the speed term.
    #[arg(long, default_value_t = 1600.0)]
    dpi: f64,

    /// Time constant for the velocity estimate feeding the speed term. Raw
    /// per-event velocity at 1000Hz is mostly quantisation noise, which shows up
    /// as gritty pressure. 0 disables smoothing, for comparison.
    #[arg(long, default_value_t = 40.0)]
    velocity_smoothing_ms: f64,
}

pub fn run(args: Args) -> Result<()> {
    let mut tablet = build_tablet().context("creating virtual tablet")?;

    // Report where it landed so classification can be inspected externally.
    match tablet.enumerate_dev_nodes_blocking() {
        Ok(nodes) => {
            for node in nodes {
                match node {
                    Ok(path) => println!("virtual tablet node: {}", path.display()),
                    Err(e) => eprintln!("node enumeration error: {e}"),
                }
            }
        }
        Err(e) => eprintln!("could not enumerate dev nodes: {e}"),
    }

    println!();
    println!("Check classification with:");
    println!("  libinput list-devices | grep -A6 'StabMouse probe'");
    println!("  grep -A6 'StabMouse probe' /proc/bus/input/devices");
    println!();

    let Some(source_path) = args.source.clone() else {
        println!("No --source given; holding the tablet open. Ctrl-C to exit.");
        // Park so the device stays alive for inspection.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    };

    let mut source = Device::open(&source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;

    println!(
        "source: {} ({})",
        source.name().unwrap_or("<unnamed>"),
        source_path.display()
    );

    if args.grab {
        source
            .grab()
            .with_context(|| format!("grabbing {}", source_path.display()))?;
        println!("source GRABBED — physical mouse no longer moves the cursor directly.");
        println!("Ctrl-C releases it (the kernel drops the grab when the fd closes).");
    } else {
        println!("source not grabbed — expect doubled cursor motion. Pass --grab for a clean test.");
    }
    println!();
    println!("Hold left button to draw. Pressure ramps in over {ATTACK_MS:.0}ms and thins with speed.");

    drive(&mut source, &mut tablet, &args)
}

fn build_tablet() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_TOOL_PEN);
    keys.insert(KeyCode::BTN_TOUCH);
    keys.insert(KeyCode::BTN_STYLUS);

    // AbsInfo: value, min, max, fuzz, flat, resolution.
    // Resolution is in units/mm for ABS_X/Y, which libinput uses for sizing.
    let x = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_X,
        AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100),
    );
    let y = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_Y,
        AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100),
    );
    let pressure = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_PRESSURE,
        AbsInfo::new(0, 0, PRESSURE_MAX, 0, 0, 0),
    );

    let device = VirtualDevice::builder()?
        .name("StabMouse probe tablet")
        .with_keys(&keys)?
        .with_absolute_axis(&x)?
        .with_absolute_axis(&y)?
        .with_absolute_axis(&pressure)?
        .build()?;

    Ok(device)
}

/// Per-stroke state for the probe's pressure synthesis.
struct Stroke {
    started: Instant,
    down: bool,
}

fn drive(source: &mut Device, tablet: &mut VirtualDevice, args: &Args) -> Result<()> {
    let mut x = ABS_MAX / 2;
    let mut y = ABS_MAX / 2;
    let mut stroke = Stroke {
        started: Instant::now(),
        down: false,
    };

    // Accumulated within one SYN_REPORT.
    let mut dx = 0i32;
    let mut dy = 0i32;
    let mut last_event = Instant::now();
    let mut in_proximity = false;

    // Low-passed speed in mm/s feeding the pressure speed term.
    let mut speed = 0.0f64;

    // Last emitted values, so unchanged axes are not restated every report.
    let mut last_x = x;
    let mut last_y = y;
    let mut last_pressure = -1i32;
    let mut last_touch = false;

    let counts_per_mm = args.dpi / 25.4;

    loop {
        for event in source.fetch_events().context("reading source")? {
            match event.event_type() {
                EventType::RELATIVE => {
                    let code = RelativeAxisCode(event.code());
                    if code == RelativeAxisCode::REL_X {
                        dx += event.value();
                    } else if code == RelativeAxisCode::REL_Y {
                        dy += event.value();
                    }
                }
                EventType::KEY => {
                    if KeyCode(event.code()) == KeyCode::BTN_LEFT {
                        let pressed = event.value() != 0;
                        if pressed && !stroke.down {
                            stroke.started = Instant::now();
                        }
                        stroke.down = pressed;
                    }
                }
                EventType::SYNCHRONIZATION => {
                    // Enter proximity on first motion so the pen appears.
                    if !in_proximity {
                        tablet.emit(&[InputEvent::new(
                            EventType::KEY.0,
                            KeyCode::BTN_TOOL_PEN.code(),
                            1,
                        )])?;
                        in_proximity = true;
                    }

                    let now = Instant::now();
                    let dt = now.duration_since(last_event).as_secs_f64().max(1e-4);
                    last_event = now;

                    x = (x + (dx as f64 * args.scale) as i32).clamp(0, ABS_MAX);
                    y = (y + (dy as f64 * args.scale) as i32).clamp(0, ABS_MAX);

                    // Instantaneous speed is dominated by quantisation noise at high
                    // polling rates, so low-pass it before it reaches the speed term.
                    let dist_mm = ((dx as f64).hypot(dy as f64)) / counts_per_mm;
                    let raw_speed = dist_mm / dt;
                    let tau = args.velocity_smoothing_ms / 1000.0;
                    speed = if tau <= 0.0 {
                        raw_speed
                    } else {
                        let alpha = 1.0 - (-dt / tau).exp();
                        speed + alpha * (raw_speed - speed)
                    };

                    let pressure = if stroke.down {
                        let elapsed_ms = now.duration_since(stroke.started).as_secs_f64() * 1000.0;
                        let envelope = (elapsed_ms / ATTACK_MS).min(1.0);
                        let speed_term = (1.0 - speed / V_MAX_MM_S).clamp(0.0, 1.0);
                        let p = (envelope * speed_term).max(MIN_PRESSURE);
                        (p * PRESSURE_MAX as f64) as i32
                    } else {
                        0
                    };

                    // Emit only what changed. Restating every axis on every report
                    // is an event storm, and may be behind the doubled clicks seen
                    // in application menus.
                    let mut out = Vec::with_capacity(4);
                    if x != last_x {
                        out.push(InputEvent::new(
                            EventType::ABSOLUTE.0,
                            AbsoluteAxisCode::ABS_X.0,
                            x,
                        ));
                        last_x = x;
                    }
                    if y != last_y {
                        out.push(InputEvent::new(
                            EventType::ABSOLUTE.0,
                            AbsoluteAxisCode::ABS_Y.0,
                            y,
                        ));
                        last_y = y;
                    }
                    if pressure != last_pressure {
                        out.push(InputEvent::new(
                            EventType::ABSOLUTE.0,
                            AbsoluteAxisCode::ABS_PRESSURE.0,
                            pressure,
                        ));
                        last_pressure = pressure;
                    }
                    if stroke.down != last_touch {
                        out.push(InputEvent::new(
                            EventType::KEY.0,
                            KeyCode::BTN_TOUCH.code(),
                            i32::from(stroke.down),
                        ));
                        last_touch = stroke.down;
                    }

                    if !out.is_empty() {
                        tablet.emit(&out)?;
                    }

                    dx = 0;
                    dy = 0;
                }
                _ => {}
            }
        }
    }
}
