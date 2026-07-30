//! Passthrough probe (P4).
//!
//! Grabs a real mouse, replicates its capabilities on a virtual device, and
//! forwards events through — the shape the real daemon will have. Answers:
//!
//! - Do games see the virtual device correctly, given the real one is grabbed and
//!   disappears from their point of view?
//! - Does Steam Input interfere?
//! - **Does an application notice if the virtual device stays alive but goes
//!   silent?** That is the open `Enabled: off` question in ux-requirements.md.
//!
//! Press Enter to toggle forwarding without destroying the device, which is exactly
//! the `Enabled: off` case. Ctrl-C exits and the kernel releases the grab.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::VirtualDevice, AttributeSet, Device, EventType, InputEvent, KeyCode, RelativeAxisCode,
};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(ClapArgs)]
pub struct Args {
    /// Source mouse, e.g. /dev/input/event2.
    #[arg(long)]
    source: PathBuf,

    /// Sensitivity multiplier. Anything other than 1.0 makes it obvious the probe
    /// is genuinely in the path rather than being bypassed.
    #[arg(long, default_value_t = 1.0)]
    sens: f64,

    /// Forward without grabbing the source. Produces doubled motion, but cannot
    /// strand you without a pointer.
    #[arg(long)]
    no_grab: bool,
}

pub fn run(args: Args) -> Result<()> {
    let mut source = Device::open(&args.source)
        .with_context(|| format!("opening {}", args.source.display()))?;

    let name = source.name().unwrap_or("<unnamed>").to_owned();
    println!("source: {name} ({})", args.source.display());

    // Replicate every capability the source has. Dropping one silently degrades
    // the device — losing hi-res scroll is the classic version of this.
    let mut keys = AttributeSet::<KeyCode>::new();
    if let Some(k) = source.supported_keys() {
        for key in k.iter() {
            keys.insert(key);
        }
    }
    let mut rel = AttributeSet::<RelativeAxisCode>::new();
    if let Some(axes) = source.supported_relative_axes() {
        for axis in axes.iter() {
            rel.insert(axis);
        }
    }

    println!(
        "replicating {} buttons, {} relative axes",
        keys.iter().count(),
        rel.iter().count()
    );

    let mut sink = VirtualDevice::builder()?
        .name(&format!("StabMouse passthrough ({name})"))
        .with_keys(&keys)?
        .with_relative_axes(&rel)?
        .build()?;

    if let Ok(nodes) = sink.enumerate_dev_nodes_blocking() {
        for node in nodes.flatten() {
            println!("virtual device: {}", node.display());
        }
    }

    if args.no_grab {
        println!("NOT grabbed — expect doubled motion.");
    } else {
        source
            .grab()
            .with_context(|| format!("grabbing {}", args.source.display()))?;
        println!("source grabbed.");
    }

    println!();
    println!("sens = {:.2}", args.sens);
    println!("Press Enter to toggle forwarding (device stays alive either way).");
    println!("Ctrl-C to exit.");
    println!();

    let forwarding = Arc::new(AtomicBool::new(true));
    {
        let forwarding = forwarding.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for _ in stdin.lock().lines() {
                let now = !forwarding.load(Ordering::Relaxed);
                forwarding.store(now, Ordering::Relaxed);
                println!(
                    "forwarding {} — virtual device still present",
                    if now { "ON" } else { "OFF" }
                );
            }
        });
    }

    // Subpixel remainder. Without this, any sens below 1.0 truncates to zero on
    // small movements and slow motion is silently lost -- see stages.md.
    let mut carry_x = 0.0f64;
    let mut carry_y = 0.0f64;

    let mut pending: Vec<InputEvent> = Vec::with_capacity(8);

    loop {
        for event in source.fetch_events().context("reading source")? {
            if !forwarding.load(Ordering::Relaxed) {
                continue;
            }

            match event.event_type() {
                EventType::RELATIVE => {
                    let code = RelativeAxisCode(event.code());
                    if code == RelativeAxisCode::REL_X || code == RelativeAxisCode::REL_Y {
                        let carry = if code == RelativeAxisCode::REL_X {
                            &mut carry_x
                        } else {
                            &mut carry_y
                        };
                        let scaled = event.value() as f64 * args.sens + *carry;
                        let out = scaled.trunc();
                        *carry = scaled - out;
                        if out != 0.0 {
                            pending.push(InputEvent::new(
                                EventType::RELATIVE.0,
                                event.code(),
                                out as i32,
                            ));
                        }
                    } else {
                        // Wheel, hi-res wheel, pan: forward untouched.
                        pending.push(InputEvent::new(
                            EventType::RELATIVE.0,
                            event.code(),
                            event.value(),
                        ));
                    }
                }
                EventType::KEY => {
                    pending.push(InputEvent::new(
                        EventType::KEY.0,
                        event.code(),
                        event.value(),
                    ));
                }
                EventType::SYNCHRONIZATION => {
                    if !pending.is_empty() {
                        sink.emit(&pending).context("emitting to sink")?;
                        pending.clear();
                    }
                }
                _ => {}
            }
        }
    }
}
