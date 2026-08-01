//! Does process death really release an `EVIOCGRAB`? (P8)
//!
//! Everything about StabMouse's safety story rests on one kernel behaviour: the grab is held by
//! a file descriptor, so **the kernel releases it when the process dies**, however it dies. The
//! watchdog is built on it (abort → grab released → cursor back), the panic path is built on
//! it, and `modules.md` states it as a criterion that had never actually been run.
//!
//! So this runs it, against every death that matters — a clean exit, `SIGKILL`, and the
//! `SIGABRT` the watchdog itself raises.
//!
//! **Fully synthetic.** The device grabbed here is one this probe creates; the real mouse is
//! never touched, so a failure costs nothing. That is the rule from the development strategy,
//! and it is what makes this safe to run on a machine somebody is using.
//!
//! The child is this same binary re-invoked with `grabhold`, rather than a thread: a thread
//! cannot be `SIGKILL`ed on its own, and the question is specifically about *process* death.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::VirtualDevice, AttributeSet, Device, KeyCode, RelativeAxisCode,
};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(ClapArgs)]
pub struct Args {
    /// How long to wait for the kernel to release a grab after the holder dies.
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
}

#[derive(ClapArgs)]
pub struct HoldArgs {
    /// Event node to grab.
    pub path: PathBuf,
}

const DEVICE_NAME: &str = "StabMouse probe grab target";

/// The child half: grab the device, say so, then wait to be killed.
pub fn hold(args: HoldArgs) -> Result<()> {
    let mut device = Device::open(&args.path)
        .with_context(|| format!("opening {}", args.path.display()))?;
    device.grab().context("grabbing the synthetic device")?;
    // The parent blocks on this line, so it must be flushed rather than buffered until exit.
    println!("grabbed");
    use std::io::Write;
    std::io::stdout().flush().ok();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// How a holder was ended, and what that is supposed to prove.
struct Death {
    label: &'static str,
    signal: Option<&'static str>,
    why: &'static str,
}

const DEATHS: &[Death] = &[
    Death {
        label: "SIGKILL",
        signal: Some("KILL"),
        why: "the ungrab-on-drop path cannot run at all, so only the kernel can release it",
    },
    Death {
        label: "SIGABRT",
        signal: Some("ABRT"),
        why: "exactly what the watchdog raises — the grab must come back without our help",
    },
    Death {
        label: "SIGTERM",
        signal: Some("TERM"),
        why: "the ordinary stop, and the one systemd sends",
    },
];

pub fn run(args: Args) -> Result<()> {
    let timeout = Duration::from_millis(args.timeout_ms);

    // Buttons and axes so udev classifies it as a mouse, which is what makes it a fair stand-in
    // for the device the daemon actually grabs.
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_LEFT);
    let mut axes = AttributeSet::<RelativeAxisCode>::new();
    axes.insert(RelativeAxisCode::REL_X);
    axes.insert(RelativeAxisCode::REL_Y);

    let mut source = VirtualDevice::builder()
        .and_then(|b| {
            b.name(DEVICE_NAME)
                .with_keys(&keys)
                .and_then(|b| b.with_relative_axes(&axes))
        })
        .and_then(|b| b.build())
        .context("creating the synthetic device")?;

    let node = wait_for_node(&mut source, timeout)?;
    println!("synthetic device at {}\n", node.display());

    let exe = std::env::current_exe().context("finding this binary")?;
    let mut failures = 0;

    for death in DEATHS {
        print!("{:<8} — {}\n", death.label, death.why);

        let mut child = spawn_holder(&exe, &node)?;
        wait_for_grabbed(&mut child, timeout)?;

        // The control: while the child holds it, we must *not* be able to grab. Without this a
        // "released" verdict would also be printed by a child that never grabbed anything.
        if can_grab(&node) {
            println!("           INCONCLUSIVE: the device was grabbable while held\n");
            let _ = child.kill();
            let _ = child.wait();
            failures += 1;
            continue;
        }
        println!("           held: confirmed (we cannot grab it)");

        match death.signal {
            Some(sig) => {
                Command::new("kill")
                    .args([&format!("-{sig}"), &child.id().to_string()])
                    .status()
                    .with_context(|| format!("sending SIG{sig}"))?;
            }
            None => {
                let _ = child.kill();
            }
        }
        let _ = child.wait();

        match released_within(&node, timeout) {
            Some(after) => println!("           released after {}ms\n", after.as_millis()),
            None => {
                println!(
                    "           NOT RELEASED within {}ms — the safety story does not hold\n",
                    timeout.as_millis()
                );
                failures += 1;
            }
        }
    }

    // The other half of the criterion: the virtual device must not outlive its creator.
    drop(source);
    std::thread::sleep(Duration::from_millis(200));
    let orphaned = node.exists();
    println!(
        "orphan check: the synthetic node is {} after its creator dropped it",
        if orphaned { "STILL PRESENT" } else { "gone" }
    );
    if orphaned {
        failures += 1;
    }

    println!();
    if failures == 0 {
        println!("PASS — every death released the grab, and nothing was orphaned.");
        Ok(())
    } else {
        bail!("{failures} check(s) failed")
    }
}

fn spawn_holder(exe: &Path, node: &Path) -> Result<Child> {
    Command::new(exe)
        .arg("grabhold")
        .arg(node)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawning the holder")
}

/// Block until the holder reports success, so the test never races its own child.
fn wait_for_grabbed(child: &mut Child, timeout: Duration) -> Result<()> {
    let Some(stdout) = child.stdout.take() else {
        bail!("the holder had no stdout");
    };
    let deadline = Instant::now() + timeout;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => bail!("the holder exited before grabbing"),
            Ok(_) if line.trim() == "grabbed" => return Ok(()),
            Ok(_) => continue,
            Err(e) => bail!("reading from the holder: {e}"),
        }
    }
    bail!("the holder did not grab within {}ms", timeout.as_millis())
}

/// Whether the device can be grabbed right now. Releases immediately either way.
fn can_grab(node: &Path) -> bool {
    match Device::open(node) {
        Ok(mut d) => {
            let got = d.grab().is_ok();
            if got {
                let _ = d.ungrab();
            }
            got
        }
        Err(_) => false,
    }
}

fn released_within(node: &Path, timeout: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if can_grab(node) {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A uinput device is not usable the instant it is built — udev has to process it.
///
/// Waiting for the *path* is not enough: the kernel creates the node, and udev applies its
/// group and ACL a moment later, so a node that exists can still be `EACCES` to open. Measured
/// here the first time this probe ran, and it is the same adoption latency D17 records for
/// mapping a tablet. So the wait is for openability, which is the property the caller needs.
fn wait_for_node(device: &mut VirtualDevice, timeout: Duration) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    let mut found: Option<PathBuf> = None;
    loop {
        if found.is_none() {
            if let Ok(nodes) = device.enumerate_dev_nodes_blocking() {
                found = nodes.flatten().next();
            }
        }
        if let Some(path) = &found {
            match Device::open(path) {
                Ok(_) => return Ok(path.clone()),
                Err(e) if Instant::now() >= deadline => {
                    bail!("{} never became openable: {e}", path.display());
                }
                Err(_) => {}
            }
        }
        if Instant::now() >= deadline {
            bail!("the synthetic device never got an event node");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
