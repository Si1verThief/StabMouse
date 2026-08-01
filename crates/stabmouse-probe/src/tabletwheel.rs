//! Can one device be both a tablet and a scroll wheel? (P9)
//!
//! Krita discards wheel events that arrive while a pen is in proximity — a defence against
//! drivers that synthesise mouse input from tablet input, which would otherwise double every
//! action. Our wheel comes from a genuinely separate device, but Krita cannot tell that, so
//! scrolling only works while the pen holds still (measured in use, 2026-08-01).
//!
//! The obvious escape is to stop being two devices: put `REL_WHEEL` on the tablet itself, the
//! way a real tablet carries a ring or a wheel alongside its stylus.
//!
//! **The risk is the reason this is a probe and not a patch.** Tablet classification hangs off
//! udev seeing `BTN_TOOL_PEN` with absolute axes, and adding relative axes may make udev call
//! the device a mouse as well — or instead. If libinput then declines to treat it as a tablet
//! tool, drawing stops working altogether, which is a far worse outcome than a wheel that does
//! not scroll. So the classification is checked before the daemon is allowed to depend on it.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent,
    KeyCode, RelativeAxisCode, UinputAbsSetup,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// Seconds to leave the device present, for watching what applications make of it.
    #[arg(long, default_value_t = 0)]
    hold_seconds: u64,
}

const ABS_MAX: i32 = 65535;
const PRESSURE_MAX: i32 = 4095;

/// Build a tablet, optionally carrying a wheel.
fn build(name: &str, with_wheel: bool) -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_TOOL_PEN);
    keys.insert(KeyCode::BTN_TOUCH);
    keys.insert(KeyCode::BTN_STYLUS);

    let x = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_X,
        AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100),
    );
    let y = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_Y,
        AbsInfo::new(0, 0, ABS_MAX, 0, 0, 100),
    );
    let p = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_PRESSURE,
        AbsInfo::new(0, 0, PRESSURE_MAX, 0, 0, 0),
    );

    let mut builder = VirtualDevice::builder()?
        .name(name)
        .with_keys(&keys)?
        .with_absolute_axis(&x)?
        .with_absolute_axis(&y)?
        .with_absolute_axis(&p)?;

    if with_wheel {
        let mut rel = AttributeSet::<RelativeAxisCode>::new();
        rel.insert(RelativeAxisCode::REL_WHEEL);
        // Hi-res too: whole-notch scrolling feels broken for anything continuous, and a device
        // that advertises only the coarse axis cannot be fixed later without recreating it.
        rel.insert(RelativeAxisCode::REL_WHEEL_HI_RES);
        builder = builder.with_relative_axes(&rel)?;
    }

    Ok(builder.build()?)
}

/// What udev decided about a device node.
fn udev_properties(node: &Path) -> Vec<(String, String)> {
    let Ok(out) = Command::new("udevadm")
        .args(["info", "--query=property", "--name"])
        .arg(node)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.starts_with("ID_INPUT"))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn has(props: &[(String, String)], key: &str) -> bool {
    props.iter().any(|(k, v)| k == key && v == "1")
}

/// Whether the compositor lists the device among its tablet tools.
fn kwin_sees_tablet(name: &str) -> Option<bool> {
    let tools = stabmouse_desktop::kde::tablet_tools().ok()?;
    Some(tools.iter().any(|t| t == name))
}

fn wait_for_node(device: &mut VirtualDevice, timeout: Duration) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(nodes) = device.enumerate_dev_nodes_blocking() {
            if let Some(path) = nodes.flatten().next() {
                // udev applies properties a moment after the node appears (P8), so a query
                // fired immediately would read an unclassified device.
                std::thread::sleep(Duration::from_millis(250));
                return Ok(path);
            }
        }
        if Instant::now() >= deadline {
            bail!("the device never got an event node");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Verdict {
    tablet: bool,
    mouse: bool,
    kwin_tool: Option<bool>,
}

fn examine(label: &str, with_wheel: bool) -> Result<Verdict> {
    let name = format!("StabMouse probe {label}");
    let mut device = build(&name, with_wheel).context("creating the device")?;
    let node = wait_for_node(&mut device, Duration::from_secs(2))?;
    let props = udev_properties(&node);

    // Put the tool in proximity: KWin only lists a tablet tool it has actually seen.
    device.emit(&[
        InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOOL_PEN.code(), 1),
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, ABS_MAX / 2),
        InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, ABS_MAX / 2),
    ])?;
    std::thread::sleep(Duration::from_millis(400));
    let kwin_tool = kwin_sees_tablet(&name);
    device.emit(&[InputEvent::new(
        EventType::KEY.0,
        KeyCode::BTN_TOOL_PEN.code(),
        0,
    )])?;

    let verdict = Verdict {
        tablet: has(&props, "ID_INPUT_TABLET"),
        mouse: has(&props, "ID_INPUT_MOUSE"),
        kwin_tool,
    };

    println!("{label}:");
    println!("  node                 {}", node.display());
    let listed: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("  udev                 {}", listed.join(" "));
    println!(
        "  ID_INPUT_TABLET      {}",
        if verdict.tablet { "yes" } else { "NO" }
    );
    println!(
        "  ID_INPUT_MOUSE       {}",
        if verdict.mouse { "yes" } else { "no" }
    );
    println!(
        "  KWin lists as tablet {}\n",
        match verdict.kwin_tool {
            Some(true) => "yes",
            Some(false) => "NO",
            None => "could not ask",
        }
    );
    Ok(verdict)
}

pub fn run(args: Args) -> Result<()> {
    println!("Does adding a wheel cost a tablet its classification?\n");

    let plain = examine("tablet-plain", false)?;
    let wheeled = examine("tablet-with-wheel", true)?;

    println!("verdict:");
    if !plain.tablet {
        println!("  INCONCLUSIVE — even the plain tablet did not classify, so nothing here");
        println!("  can be attributed to the wheel.");
        return Ok(());
    }

    let kept_udev = wheeled.tablet;
    let kept_kwin = wheeled.kwin_tool.unwrap_or(true);
    if kept_udev && kept_kwin {
        println!("  SAFE — a tablet keeps its classification with a wheel attached.");
        if wheeled.mouse && !plain.mouse {
            println!("  It now *also* classifies as a mouse, which is what a real tablet with a");
            println!("  wheel does. Worth knowing when enumerating devices, harmless otherwise.");
        }
        println!("\n  Next: route the wheel out of the tablet and test scrolling in Krita, which");
        println!("  is the only application known to reject the two-device arrangement.");
    } else {
        println!("  UNSAFE — adding a wheel cost the device its tablet identity:");
        if !kept_udev {
            println!("    udev no longer sets ID_INPUT_TABLET");
        }
        if !kept_kwin {
            println!("    KWin no longer lists it as a tablet tool");
        }
        println!("  Drawing would stop working. Do not put a wheel on the tablet sink;");
        println!("  freeze the pen position while scrolling instead.");
    }

    if args.hold_seconds > 0 {
        println!("\nholding both devices for {}s", args.hold_seconds);
        std::thread::sleep(Duration::from_secs(args.hold_seconds));
    }
    Ok(())
}
