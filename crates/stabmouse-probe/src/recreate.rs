//! How long a virtual tablet takes to become usable after being created.
//!
//! This exists to answer one question with a number: is destroying the tablet on leaving
//! tablet mode cheap enough to offer as an option? Krita keeps painting a stale canvas
//! cursor after a proximity-out (see D13), and a device that goes away entirely forces it
//! to reset — but only if coming back is fast enough not to be felt.
//!
//! **The number that matters is not how long `uinput` takes.** Creating the device is a
//! syscall and is always fast. What the user waits for is the whole chain: udev processing
//! the new device, libinput classifying it, and the compositor adopting it. So this measures
//! creation through to the point where KWin exposes the device on D-Bus *and* agrees it is a
//! tablet tool — the first instant an application could actually receive pressure from it.
//!
//! Teardown is measured the same way, to when KWin drops the object.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use stabmouse_output::TabletSink;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// How many create/destroy cycles to time.
    #[arg(long, default_value_t = 10)]
    cycles: usize,

    /// Give up on a cycle after this many milliseconds.
    #[arg(long, default_value_t = 5000)]
    timeout_ms: u64,
}

/// Ask KWin whether it holds this device and considers it a tablet tool.
///
/// Presence of the object alone is not enough. KWin creates the D-Bus object as soon as
/// libinput hands it the device, but a client only gets pressure once it is classified as a
/// tablet tool — so that property is the honest definition of "ready".
fn kwin_sees_tablet(sys_name: &str) -> bool {
    let path = format!("/org/kde/KWin/InputDevice/{sys_name}");
    let out = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.kde.KWin",
            &path,
            "org.kde.KWin.InputDevice",
            "tabletTool",
        ])
        .stderr(Stdio::null())
        .output();

    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).contains("true"),
        _ => false,
    }
}

fn kwin_has_object(sys_name: &str) -> bool {
    let path = format!("/org/kde/KWin/InputDevice/{sys_name}");
    Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.kde.KWin",
            &path,
            "org.kde.KWin.InputDevice",
            "name",
        ])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll `check` until it matches `want`, reporting elapsed time measured from `since`.
///
/// The caller supplies the origin so the result covers everything the daemon would have to
/// do — including resolving the device node — rather than only the polling loop.
fn wait_until(
    since: Instant,
    want: bool,
    timeout: Duration,
    mut check: impl FnMut() -> bool,
) -> Option<Duration> {
    let start = Instant::now();
    loop {
        if check() == want {
            return Some(since.elapsed());
        }
        if start.elapsed() > timeout {
            return None;
        }
        // Each probe is a busctl process, so polling faster than this measures process
        // spawn cost rather than device readiness.
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn run(args: Args) -> Result<()> {
    if Command::new("busctl").arg("--version").stdout(Stdio::null()).status().is_err() {
        bail!("busctl not found; this probe reads KWin's D-Bus interface");
    }

    let timeout = Duration::from_millis(args.timeout_ms);
    let mut up: Vec<Duration> = Vec::new();
    let mut down: Vec<Duration> = Vec::new();

    println!("cycle   create->usable   destroy->gone   node");
    for i in 0..args.cycles {
        // A fresh name each cycle would defeat the point: KWin keys per-device settings off
        // the name, and the real feature reuses one identity.
        let mut tablet = TabletSink::new("StabMouse tablet recreate probe")
            .context("creating the virtual tablet")?;
        let created = Instant::now();

        let node = tablet
            .nodes()
            .into_iter()
            .find_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .context("the new tablet reported no device node")?;

        let up_ms = match wait_until(created, true, timeout, || kwin_sees_tablet(&node)) {
            Some(d) => {
                up.push(d);
                format!("{:>8.1} ms", d.as_secs_f64() * 1000.0)
            }
            None => "  timeout".to_string(),
        };

        let destroyed = Instant::now();
        drop(tablet);
        let gone = wait_until(destroyed, false, timeout, || kwin_has_object(&node));
        let down_ms = match gone {
            Some(d) => {
                down.push(d);
                format!("{:>8.1} ms", d.as_secs_f64() * 1000.0)
            }
            None => "  timeout".to_string(),
        };

        println!("{:>5}   {up_ms}   {down_ms}   {node}", i + 1);

        // Back-to-back create/destroy is not the pattern being measured, and it risks udev
        // coalescing the events. A mode switch is a human action.
        std::thread::sleep(Duration::from_millis(200));
    }

    summarise("create -> usable", &up, args.cycles);
    summarise("destroy -> gone", &down, args.cycles);

    println!(
        "\nA switch out of and back into tablet mode costs roughly one create.\n\
         Under ~100ms reads as instant; over ~300ms will be felt as a hitch."
    );
    Ok(())
}

fn summarise(label: &str, samples: &[Duration], expected: usize) {
    if samples.is_empty() {
        println!("\n{label}: no successful samples");
        return;
    }
    let mut ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;

    // Worst case is what a user notices, so it is reported alongside the median rather than
    // being averaged away.
    println!(
        "\n{label}: median {:.1} ms, mean {:.1} ms, min {:.1} ms, max {:.1} ms  ({}/{} cycles)",
        ms[ms.len() / 2],
        mean,
        ms[0],
        ms[ms.len() - 1],
        ms.len(),
        expected
    );
}
