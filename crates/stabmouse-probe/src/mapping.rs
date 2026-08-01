//! Whether a tablet can be confined to one screen from outside the desktop's own settings.
//!
//! The multi-monitor design gives every screen its own tablet, which is only worth building if
//! placing them can be done automatically — a design that requires the user to visit System
//! Settings once per monitor is a different, worse feature.
//!
//! This creates a throwaway tablet and maps it, rather than touching a tablet the daemon is
//! using. Mapping a live device would move the user's cursor onto another screen mid-stroke.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use stabmouse_output::TabletSink;
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// Screen to map onto. Defaults to the first one reported.
    #[arg(long)]
    output: Option<String>,
}

const PROBE_NAME: &str = "StabMouse tablet mapping probe";

pub fn run(args: Args) -> Result<()> {
    let outputs = stabmouse_desktop::outputs().context("listing screens")?;
    if outputs.is_empty() {
        anyhow::bail!("the compositor reported no screens");
    }
    for o in &outputs {
        println!("screen {:<12} {}x{} at {},{}", o.name, o.width, o.height, o.x, o.y);
    }

    let target = match &args.output {
        Some(name) => outputs
            .iter()
            .find(|o| &o.name == name)
            .with_context(|| format!("no screen named {name}"))?,
        None => &outputs[0],
    };

    if !stabmouse_desktop::can_map_tablets() {
        anyhow::bail!("this desktop cannot map tablets from outside its own settings");
    }

    let tablet = TabletSink::new(PROBE_NAME).context("creating the probe tablet")?;
    println!("\ncreated {PROBE_NAME:?}");

    // The compositor has to adopt the device before it can be addressed by name; ~50ms on this
    // host, so a short wait rather than an immediate attempt.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if stabmouse_desktop::kde::mapped_output(PROBE_NAME).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let before = stabmouse_desktop::kde::mapped_output(PROBE_NAME)?;
    println!("before: {}", describe(&before));

    stabmouse_desktop::map_tablet(PROBE_NAME, &target.name)
        .with_context(|| format!("mapping onto {}", target.name))?;

    let after = stabmouse_desktop::kde::mapped_output(PROBE_NAME)?;
    println!("after:  {}", describe(&after));

    // Gone before the verdict is printed, so a failing probe does not leave a stray tablet
    // behind for the user to wonder about.
    drop(tablet);

    if after.as_deref() == Some(target.name.as_str()) {
        println!("\nautomatic per-screen placement works: the mapping took effect immediately");
        Ok(())
    } else {
        anyhow::bail!(
            "the mapping did not take: asked for {}, compositor reports {}",
            target.name,
            describe(&after)
        )
    }
}

fn describe(mapped: &Option<String>) -> String {
    mapped.clone().unwrap_or_else(|| "the whole desktop".into())
}
