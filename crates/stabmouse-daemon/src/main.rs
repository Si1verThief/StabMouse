//! The StabMouse daemon.
//!
//! Deliberately usable before the GUI exists: point it at a config directory, edit a
//! preset in a text editor, and the change takes effect within about half a second. That
//! is the tuning loop, and it does not need D-Bus to work.

mod auto;
mod control;
mod focus;
mod modes;
mod notify;
mod pidfile;
mod runtime;
mod service;
mod signals;
mod starter;
mod tablet;
mod tablets;
mod wait;
mod watch;
mod watchdog;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use modes::{Mode, Modes};
use runtime::Runtime;
use signals::Signals;
use stabmouse_config::{Identity, Store};
use stabmouse_core::Quantizer;
use stabmouse_input::{Capture, Kind};
use stabmouse_output::{MouseSink, SurfaceMapper};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "stabmoused", about = "StabMouse input daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Config directory. Defaults to $XDG_CONFIG_HOME/stabmouse.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[arg(long, short, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List input devices and how they would be treated.
    Devices,
    /// Write a starter config directory.
    Init {
        /// Overwrite files that already exist.
        #[arg(long)]
        force: bool,
    },
    /// Switch mode on the running daemon. Bind this to a hotkey.
    Switch {
        /// Previous slot instead of next.
        #[arg(long)]
        prev: bool,
        /// Back and forth between the last two slots, instead of cycling.
        ///
        /// Cycling is the default because it never depends on timing: a gesture you cannot
        /// distinguish from two separate presses reads as unreliable even when it works.
        #[arg(long, conflicts_with = "prev")]
        flip: bool,
    },
    /// Jump to a specific mode slot, 1-based.
    Mode {
        slot: usize,
    },
    /// List the screens, and what the compositor believes about our tablets.
    Screens,
    /// Ask the running daemon what it is doing.
    Status,
    /// Panic on the running daemon: release the grab and stop. Again to resume.
    Panic,
    /// Ask the running daemon to exit.
    Quit,
    /// Run the daemon.
    Run {
        /// Source device. Defaults to the managed device found in config.
        #[arg(long)]
        device: Option<PathBuf>,
        /// Profile slug, overriding the configured default.
        #[arg(long)]
        profile: Option<String>,
        /// Mode slot, 1-based, overriding the profile's default.
        #[arg(long)]
        mode: Option<usize>,
        /// Read and filter without grabbing. Motion is doubled, but nothing can leave you
        /// without a pointer — the safe way to try an unfamiliar config.
        #[arg(long)]
        no_grab: bool,
        /// Abort if one unit of work takes longer than this, releasing the grab. 0 disables.
        ///
        /// The daemon holds an exclusive grab on the pointing device, so a wedged loop means
        /// no cursor. Dying is how the grab comes back — see watchdog.rs.
        #[arg(long, default_value_t = crate::watchdog::DEFAULT_TIMEOUT.as_millis() as u64)]
        watchdog_ms: u64,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dir = cli.config.clone().unwrap_or_else(default_config_dir);

    // No subcommand lists devices rather than running: the safe default for a program
    // whose main action is taking exclusive control of the user's mouse.
    match cli.command.unwrap_or(Command::Devices) {
        Command::Devices => {
            list_devices()?;
            println!();
            println!("`stabmoused init` writes a starter config; `stabmoused run` starts filtering.");
            Ok(())
        }
        Command::Init { force } => starter::write(&dir, force),
        Command::Switch { prev, flip } => control::send(&if flip {
            control::Command::Flip
        } else if prev {
            control::Command::CyclePrev
        } else {
            control::Command::Cycle
        }),
        Command::Mode { slot } => control::send(&control::Command::Mode(slot)),
        Command::Screens => screens(),
        Command::Status => control::send(&control::Command::Status),
        Command::Panic => control::send(&control::Command::Panic),
        Command::Quit => control::send(&control::Command::Quit),
        Command::Run {
            device,
            profile,
            mode,
            no_grab,
            watchdog_ms,
        } => run(
            dir,
            device,
            profile,
            mode,
            no_grab,
            cli.verbose,
            std::time::Duration::from_millis(watchdog_ms),
        ),
    }
}

fn default_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("stabmouse")
}

/// Report the screen layout and what the compositor believes about our tablets.
///
/// Both halves matter and they fail independently. Screens come from `wl_output`, which works
/// on any Wayland compositor; confining a tablet to one is a KDE-specific setting with no
/// standard equivalent. Saying which of the two is unavailable is the difference between a
/// user knowing to map it by hand and concluding the feature is broken.
fn screens() -> anyhow::Result<()> {
    match stabmouse_desktop::outputs() {
        Ok(outputs) if outputs.is_empty() => println!("the compositor reported no screens"),
        Ok(outputs) => {
            println!("screens:");
            for o in &outputs {
                println!(
                    "  {:<12} {:>5}x{:<5} at {:>5},{:<5}  aspect {:.2}{}",
                    o.name,
                    o.width,
                    o.height,
                    o.x,
                    o.y,
                    o.aspect(),
                    o.description
                        .as_deref()
                        .map(|d| format!("  ({d})"))
                        .unwrap_or_default()
                );
            }
        }
        Err(e) => println!("screens unavailable: {e}"),
    }

    println!();
    if stabmouse_desktop::can_map_tablets() {
        println!("tablet placement: automatic (KDE Plasma)");
        match stabmouse_desktop::kde::tablet_tools() {
            Ok(tools) if tools.is_empty() => {
                println!("  the compositor currently sees no tablet tools")
            }
            Ok(tools) => {
                for name in tools {
                    // What the compositor thinks, not what we asked for. Those differ exactly
                    // when something went wrong, which is when this is worth reading.
                    let on = stabmouse_desktop::kde::mapped_output(&name)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "the whole desktop".into());
                    println!("  {name} -> {on}");
                }
            }
            Err(e) => println!("  could not list tablet tools: {e}"),
        }
    } else {
        println!(
            "tablet placement: manual — this desktop has no way to confine a tablet to a screen\n\
             from outside its own settings, so map it there once."
        );
    }
    Ok(())
}

/// The source's relative axes minus its motion axes, for the absolute pointer.
///
/// A device declaring both relative and absolute motion invites libinput to classify it as
/// something other than an absolute mouse, so `REL_X`/`REL_Y` stay off it. Wheels — hi-res
/// and horizontal included — pass through, which is what lets fallback scrolling land under
/// the visible cursor.
fn wheel_axes(
    source: &evdev::AttributeSet<evdev::RelativeAxisCode>,
) -> evdev::AttributeSet<evdev::RelativeAxisCode> {
    let mut out = evdev::AttributeSet::new();
    for axis in source.iter() {
        if axis != evdev::RelativeAxisCode::REL_X && axis != evdev::RelativeAxisCode::REL_Y {
            out.insert(axis);
        }
    }
    out
}

/// The screen layout as the compositor reports it, empty when it tells us nothing.
fn query_screens() -> Vec<stabmouse_output::Screen> {
    stabmouse_desktop::outputs()
        .unwrap_or_default()
        .into_iter()
        .map(|o| stabmouse_output::Screen {
            name: o.name,
            x: o.x,
            y: o.y,
            width: o.width,
            height: o.height,
        })
        .collect()
}

/// One tablet per screen when the layout is known, otherwise a single desktop-wide one.
///
/// Falling back rather than failing: a compositor that tells us nothing about its screens is a
/// reason to behave as StabMouse did before D17, not a reason to refuse to start.
///
/// A *known* single screen takes the per-screen path too, not the desktop-wide one. It used
/// not to, and that left the mapper without a layout — so the fallback could not track the
/// pen's position on exactly the setups where tracking is simplest.
fn build_tablets(
    screens: Vec<stabmouse_output::Screen>,
    destroy_on_leave: bool,
) -> stabmouse_output::Result<tablets::Tablets> {
    if screens.is_empty() {
        return tablets::Tablets::single(200.0, destroy_on_leave);
    }

    let names: Vec<String> = screens.iter().map(|s| s.name.clone()).collect();
    let mut built = tablets::Tablets::per_screen(screens, 200.0, destroy_on_leave)?;
    built.place();
    if names.len() > 1 {
        println!("  tablets: one per screen — {}", names.join(", "));
        if !built.placed() {
            println!("           could not confine them automatically; map them in display settings");
        }
    }
    Ok(built)
}

fn list_devices() -> anyhow::Result<()> {
    println!("{:<22} {:<40} {:<18} {}", "PATH", "NAME", "ID", "TREATMENT");
    for d in stabmouse_input::enumerate() {
        let id = format!(
            "{}:{}",
            d.identity.vid.as_deref().unwrap_or("????"),
            d.identity.pid.as_deref().unwrap_or("????")
        );
        // Say plainly which devices can never be grabbed, so a user does not spend time
        // trying to configure one.
        let treatment = if d.is_keyboard_like() {
            "never grabbed (keyboard)"
        } else if d.kind == Kind::Mouse {
            "usable as a source"
        } else {
            "not a mouse"
        };
        println!(
            "{:<22} {:<40} {:<18} {}",
            d.path.display(),
            truncate(&d.name, 40),
            id,
            treatment
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>() + "…"
    }
}

fn run(
    dir: PathBuf,
    device: Option<PathBuf>,
    profile: Option<String>,
    mode: Option<usize>,
    no_grab: bool,
    verbose: bool,
    watchdog: std::time::Duration,
) -> anyhow::Result<()> {
    let (store, report) = Store::load(&dir).context("loading config")?;
    if !report.is_clean() {
        // Never fatal: a typo in one preset must not cost the user their mouse.
        for p in &report.problems {
            eprintln!("config problem: {p}");
        }
    }
    for d in store.dangling_references() {
        eprintln!("config problem: {d}");
    }
    if verbose {
        eprintln!("config: {}", dir.display());
        eprintln!(
            "  presets: {}   profiles: {}",
            store.preset_slugs().join(", "),
            store.profile_slugs().join(", ")
        );
    }

    let source = pick_source(&store, device.clone())?;
    let mut capture = Capture::open(&source.path)
        .with_context(|| format!("opening {}", source.path.display()))?;

    let slots = build_modes(&store, profile.as_deref(), &source.identity, verbose);
    if slots.is_empty() {
        bail!(
            "no usable mode found. Check that {} has a profile with at least one [[mode]]",
            dir.display()
        );
    }

    // Every sink is created before anything else and lives for the whole run. Qt only
    // initialises tablet support if a tablet exists at application startup, and never
    // retries — so creating the tablet lazily would give every already-running app no
    // pressure at all. See D13.
    let mouse = MouseSink::new(
        &format!("StabMouse ({})", source.name),
        &capture.keys(),
        &capture.relative_axes(),
    )
    .context("creating the virtual mouse")?;

    let screens = query_screens();

    // The fallback transport, absolute so its position cannot diverge from the tablet's
    // (P6/D22). Not fatal to lose: the relative fallback below still works, it merely
    // teleports on transport changes. The name is stable because compositors key per-device
    // settings on it.
    let pointer = if screens.is_empty() {
        eprintln!("no screen layout reported; the fallback will use the relative pointer");
        None
    } else {
        match stabmouse_output::PointerSink::new(
            "StabMouse pointer",
            &screens,
            &capture.keys(),
            &wheel_axes(&capture.relative_axes()),
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("no absolute pointer ({e}); the fallback will use the relative mouse");
                None
            }
        }
    };

    if no_grab {
        eprintln!("not grabbing: expect doubled motion. Nothing here can strand you.");
    } else {
        capture.grab().with_context(|| {
            format!(
                "grabbing {} — is it in use by another remapper?",
                source.path.display()
            )
        })?;
    }

    let start_index = mode.map(|m| m.saturating_sub(1)).unwrap_or_else(|| {
        store
            .startup_profile()
            .map(|p| p.default_mode.saturating_sub(1))
            .unwrap_or(0)
    });
    let modes = Modes::new(slots, start_index);
    let destroy_tablet_on_leave = store
        .startup_profile()
        .is_some_and(|p| p.destroy_tablet_on_leave);

    let tablets =
        build_tablets(screens, destroy_tablet_on_leave).context("creating the virtual tablets")?;

    let _pid_guard = pidfile::Guard::acquire()?;
    let control = control::Listener::bind()?;

    let overrides: Vec<(String, bool)> = store
        .root
        .data()
        .tablet_support
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();

    let scroll_freeze: Vec<(String, bool)> = store
        .root
        .data()
        .scroll_freeze
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();

    // Opt-in per profile, so an absent section means no auto-switching at all.
    let auto_rules: Vec<(String, usize)> = store
        .startup_profile()
        .map(|p| {
            p.auto_activate
                .iter()
                .map(|r| (r.app.clone(), r.mode))
                .collect()
        })
        .unwrap_or_default();

    let (published, announcements) = service::Published::new(service::Snapshot {
        profile: profile
            .clone()
            .or_else(|| store.startup_profile().and_then(|p| p.display_name.clone()))
            .unwrap_or_else(|| "default".into()),
        mode_slot: (start_index + 1) as u32,
        mode_name: modes
            .current()
            .map(|m| m.name.clone())
            .unwrap_or_default(),
        enabled: true,
        modes: modes.infos(),
        tablets: tablets.len() as u32,
        tablets_placed: tablets.placed(),
        tablet_overrides: overrides.clone(),
        ..Default::default()
    });

    // A bus failure is not fatal. The daemon's job is filtering input, and it does that with no
    // bus at all — a session without D-Bus should lose the CLI and GUI, not the mouse.
    let focus = focus::Focus::default();
    match service::serve(published.clone(), focus.clone()) {
        Ok(conn) => {
            println!("  D-Bus: {}", stabmouse_ipc::BUS_NAME);
            // Signals are emitted here, on their own thread. The input loop must never make a
            // D-Bus call: doing so once deadlocked the daemon against its own control socket
            // and left the user unable to move their mouse.
            service::run_emitter(conn, published.clone(), announcements);
        }
        Err(e) => {
            eprintln!("no D-Bus interface ({e}); the control socket still works");
        }
    }
    // Signals are now only for shutdown; every control action arrives on the socket, which
    // `poll` sees immediately instead of after a timeout.
    let signals = Signals::install();

    println!(
        "StabMouse running on {} ({})",
        source.name,
        source.path.display()
    );
    // The feature marker exists so a stale binary cannot masquerade as a new one — a release
    // build from the day before was once tested as if it were the current code, and every
    // conclusion drawn from that session was about software that was no longer there.
    println!("  build: {} — unified position core (D23)", env!("CARGO_PKG_VERSION"));
    println!("  config: {}", dir.display());
    for line in modes.names() {
        println!("    {line}");
    }
    println!();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "stabmoused".into());
    println!("  Bind any of these to a hotkey (KDE: Settings > Shortcuts > Add Command):");
    println!("      {exe} switch          next mode");
    println!("      {exe} switch --prev   previous mode");
    println!("      {exe} switch --flip   back and forth between the last two");
    println!("      {exe} mode 2          jump straight to a slot");
    println!("      {exe} panic           release the grab and stop; again to resume");
    println!();
    // One pixel per count on StabMouse's own relative pointer. Since D22 the fallback is
    // absolute, so this covers only mouse modes — where the daemon's curves must be the only
    // acceleration — and the degraded relative fallback for unknown layouts. Only our device;
    // the user's real mouse keeps whatever they configured.
    let mouse_name = mouse.name().to_string();
    std::thread::spawn(move || {
        // The compositor takes ~50ms to adopt a new device, and it cannot be addressed before
        // then. Off-thread so startup is not held up by it.
        for _ in 0..40 {
            if stabmouse_desktop::device_known(&mouse_name) {
                if let Err(e) = stabmouse_desktop::set_pointer_unaccelerated(&mouse_name) {
                    eprintln!("could not flatten pointer acceleration ({e}); fallback \
                               sensitivity may differ from tablet output");
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let focus_ok = focus::install();
    if focus_ok {
        println!("  per-application transport: on");
    } else {
        println!("  per-application transport: unavailable (no focus source)");
    }

    // Stated at startup because a rule that quietly does not fire is indistinguishable from a
    // rule that is not there — and silence is the failure mode auto-switching earns least.
    if !auto_rules.is_empty() {
        println!(
            "  auto-switch: {} rule(s) — {}",
            auto_rules.len(),
            auto_rules
                .iter()
                .map(|(app, slot)| format!("{app} → mode {slot}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if !focus_ok {
            println!("               inactive: nothing can report which window you are over");
        }
    }

    // A keyboard is opened only if some mode actually binds a keyboard key — a mouse-button
    // modifier needs nothing here, and not opening a keyboard is the strongest guarantee
    // available that keystrokes are not being read. See stabmouse-input's listen module.
    let keyboard_codes: Vec<u16> = modes
        .slots()
        .iter()
        .filter_map(|m| m.modifier)
        .filter(|c| !stabmouse_input::is_mouse_button(*c))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let listener = if keyboard_codes.is_empty() {
        None
    } else {
        match stabmouse_input::Listener::watching(&keyboard_codes) {
            Ok(l) => {
                // Named out loud, every run. A daemon that has opened a keyboard should say so
                // without being asked, whatever it promises to do with it.
                println!(
                    "  constrain modifier: watching {} keyboard(s) for {} key(s), read-only, \
                     never grabbed",
                    l.raw_fds().len(),
                    keyboard_codes.len()
                );
                Some(l)
            }
            Err(e) => {
                eprintln!("  constrain modifier: {e}");
                None
            }
        }
    };

    // Started before the loop, so the very first iteration is already covered.
    let beat = watchdog::start(watchdog);

    println!("  Editing a preset reloads it within ~0.4s. Ctrl-C to stop.");

    let mut rt = Runtime {
        capture: Some(capture),
        control,
        mouse,
        pointer,
        tablets,
        modes,
        quantizer: Quantizer::new(source_dpi(&store, &source.identity)),
        mapper: SurfaceMapper::new(200.0),
        pen_button: evdev::KeyCode::BTN_LEFT.code(),
        verbose,
        emitted_motion: false,
        stall_dx: 0.0,
        stall_dy: 0.0,
        last_emit: std::time::Instant::now(),
        beat,
        listener,
        buttons_held: Vec::new(),
        last_tablet_xy: (-1, -1),
        published,
        focus,
        tablet_overrides: overrides.clone(),
        auto: auto::AutoSwitch::new(auto_rules),
        last_transport: None,
        tablet_clicks: store
            .startup_profile()
            .is_some_and(|p| p.tablet_emits_mouse_clicks),
        freeze_while_scrolling: store
            .startup_profile()
            .is_some_and(|p| p.freeze_position_while_scrolling),
        scroll_freeze_overrides: scroll_freeze.clone(),
        scroll_freeze: std::time::Duration::from_millis(
            store.startup_profile().map_or(250, |p| p.scroll_freeze_ms),
        ),
        frozen_until: None,
        inert: false,
    };

    // Re-find the device by the same route that chose it, so a node that comes back as a
    // different `eventN` is still recognised — which is the normal case after a replug.
    let reopen_dir = dir.clone();
    let reopen_explicit = device.clone();
    let reopen_grab = !no_grab;
    let reopen = move || -> Option<Capture> {
        let (store, _) = Store::load(&reopen_dir).ok()?;
        let found = pick_source(&store, reopen_explicit.clone()).ok()?;
        let mut capture = Capture::open(&found.path).ok()?;
        if reopen_grab {
            // A device that is back but still held by something else is not ready; the next
            // interval tries again rather than running ungrabbed and doubling every motion.
            capture.grab().ok()?;
        }
        Some(capture)
    };

    let reload_dir = dir.clone();
    let reload_identity = source.identity.clone();
    let reload_profile = profile.clone();
    let stats = rt.run(
        &signals,
        move || {
            // A failed reload leaves the running modes alone rather than dropping to
            // passthrough mid-stroke.
            match Store::load(&reload_dir) {
                Ok((fresh, r)) => {
                    for p in &r.problems {
                        eprintln!("config problem: {p}");
                    }
                    let rebuilt =
                        build_modes(&fresh, reload_profile.as_deref(), &reload_identity, verbose);
                    if rebuilt.is_empty() {
                        eprintln!("reloaded config has no usable mode; keeping the running one");
                        None
                    } else {
                        Some(runtime::Reloaded {
                            modes: Modes::new(rebuilt, 0),
                            destroy_tablet_on_leave: fresh
                                .startup_profile()
                                .is_some_and(|p| p.destroy_tablet_on_leave),
                            tablet_emits_mouse_clicks: fresh
                                .startup_profile()
                                .is_some_and(|p| p.tablet_emits_mouse_clicks),
                            freeze_position_while_scrolling: fresh
                                .startup_profile()
                                .is_some_and(|p| p.freeze_position_while_scrolling),
                            scroll_freeze: fresh
                                .root
                                .data()
                                .scroll_freeze
                                .iter()
                                .map(|(k, v)| (k.clone(), *v))
                                .collect(),
                            scroll_freeze_ms: fresh
                                .startup_profile()
                                .map_or(250, |p| p.scroll_freeze_ms),
                        })
                    }
                }
                Err(e) => {
                    eprintln!("config reload failed, keeping the running config: {e}");
                    None
                }
            }
        },
        move || reopen(),
        Some(dir),
    )?;

    // Detach the compositor script. Also done on the next start, so a crash here is not
    // something the user has to clean up.
    focus::remove();

    // The grab is released by `Capture`'s Drop here, and by the kernel closing the fd if
    // this process dies any other way.
    println!(
        "stopped after {} reports, {} ticks, {} mode switches; worst in-loop processing {:?}",
        stats.reports, stats.ticks, stats.switches, stats.worst_process
    );
    Ok(())
}

/// Build a pipeline for every mode slot in the active profile.
///
/// All of them, up front: mode switching must be an index change, not a config load, or the
/// headline interaction would stutter every time it is used.
fn build_modes(
    store: &Store,
    profile_slug: Option<&str>,
    identity: &Identity,
    verbose: bool,
) -> Vec<Mode> {
    let Some(profile) = profile_slug
        .and_then(|s| store.profile(s))
        .or_else(|| store.startup_profile())
    else {
        eprintln!("no profile configured; running raw passthrough on a single mode");
        return vec![Mode {
            name: "Passthrough".into(),
            output: stabmouse_config::Output::Mouse,
            preset: stabmouse_config::RAW.into(),
            pipeline: stabmouse_core::Pipeline::new(vec![]),
            modifier: None,
        }];
    };

    let view = store.root.data().view_for(identity);
    let mut out = Vec::new();

    for m in &profile.modes {
        // A missing preset falls back to raw passthrough for that slot and says so — never a
        // silent substitution, which would leave someone working with settings they did not
        // choose and cannot see.
        let Some(preset) = store.preset(&m.preset) else {
            eprintln!(
                "mode '{}' references preset '{}', which does not exist; this slot is raw \
                 passthrough",
                m.name, m.preset
            );
            out.push(Mode {
                name: m.name.clone(),
                output: m.output,
                preset: m.preset.clone(),
                pipeline: stabmouse_core::Pipeline::new(vec![]),
                modifier: None,
            });
            continue;
        };

        let assembly = stabmouse_config::assemble(preset, &m.preset, Some(&view));
        for w in &assembly.warnings {
            eprintln!("warning: {w}");
        }
        if verbose {
            let names: Vec<&str> = assembly.pipeline.stage_names().collect();
            eprintln!(
                "  mode '{}' -> {:?} via '{}': {}",
                m.name,
                m.output,
                m.preset,
                names.join(" -> ")
            );
        }
        out.push(Mode {
            name: m.name.clone(),
            output: m.output,
            preset: m.preset.clone(),
            pipeline: assembly.pipeline,
            modifier: snap_modifier(preset, &m.preset),
        });
    }
    out
}

/// The constrain modifier a preset's `snap` stage binds, if any.
///
/// Read from the raw preset rather than from the assembled pipeline: resolving a name to an
/// evdev code is platform work, and `stabmouse-config` builds for wasm and PyO3 where no such
/// table exists. The stage knows *whether* it waits for a modifier; the daemon knows *which*.
fn snap_modifier(preset: &stabmouse_config::Preset, slug: &str) -> Option<u16> {
    let entry = preset.stages.iter().find(|s| s.kind == "snap")?;
    let name = entry.params.get("modifier")?.as_str()?;
    match stabmouse_input::code_for(name) {
        Some(code) => Some(code),
        None => {
            eprintln!("preset '{slug}': '{name}' is not a key or button name; snap will not engage");
            None
        }
    }
}

struct Source {
    path: PathBuf,
    name: String,
    identity: stabmouse_config::Identity,
}

/// Choose the source device.
///
/// An explicit `--device` wins. Otherwise only devices the config marks `managed` are
/// considered — a device absent from config is never touched, which is what keeps
/// trackpads and unrelated hardware safe by default.
fn pick_source(store: &Store, explicit: Option<PathBuf>) -> anyhow::Result<Source> {
    let devices = stabmouse_input::enumerate();

    if let Some(path) = explicit {
        let found = devices.iter().find(|d| d.path == path);
        return match found {
            Some(d) => Ok(Source {
                path: d.path.clone(),
                name: d.name.clone(),
                identity: d.identity.clone(),
            }),
            None => bail!("{} is not an evdev device", path.display()),
        };
    }

    // A single physical mouse commonly presents several event nodes that all share one
    // vid:pid — the R.A.T. 8+ exposes three, including a keyboard collection for its macro
    // keys and a second mouse collection with no buttons. Matching on vid:pid alone would
    // pick one arbitrarily, and picking the keyboard node would abort startup on the grab
    // refusal rather than simply choosing correctly.
    //
    // So filter to nodes that are actually usable as a pointer source, then treat what
    // remains as the real device list.
    let managed: Vec<_> = devices
        .iter()
        .filter(|d| store.root.data().view_for(&d.identity).is_managed())
        .collect();

    if managed.is_empty() {
        bail!(
            "no managed device found in {}. Run `stabmoused devices` to see what is \
             available, then either add a [[device]] entry with `managed = true` or pass \
             `--device <path>`",
            store.dir().display()
        );
    }

    let usable: Vec<_> = managed
        .iter()
        .copied()
        .filter(|d| d.kind == Kind::Mouse && !d.is_keyboard_like())
        .collect();

    if usable.is_empty() {
        bail!(
            "{} managed node(s) matched, but none is usable as a pointer source. \
             {} — pass `--device <path>` to name one explicitly",
            managed.len(),
            managed
                .iter()
                .map(|d| format!("{} is {:?}", d.path.display(), d.kind))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    let chosen = usable[0];

    // Only genuinely distinct devices are worth mentioning; sibling nodes of one mouse are
    // not, and saying so every launch would train the user to ignore warnings.
    let distinct: std::collections::BTreeSet<_> = usable
        .iter()
        .map(|d| (d.identity.vid.clone(), d.identity.pid.clone(), d.identity.serial.clone()))
        .collect();
    if distinct.len() > 1 {
        eprintln!(
            "{} managed devices; using {}. Multi-device support is not built yet.",
            distinct.len(),
            chosen.path.display()
        );
    }

    Ok(Source {
        path: chosen.path.clone(),
        name: chosen.name.clone(),
        identity: chosen.identity.clone(),
    })
}

/// The output quantizer should match the source resolution, so a unit multiplier is exactly
/// transparent rather than subtly rescaling.
fn source_dpi(store: &Store, identity: &stabmouse_config::Identity) -> f64 {
    let view = store.root.data().view_for(identity);
    for slug in store.preset_slugs() {
        if let Some(r) = view.resolve(&format!("{slug}.normalize.dpi")) {
            if let Some(v) = r.value.as_float().or_else(|| r.value.as_integer().map(|i| i as f64)) {
                return v;
            }
        }
    }
    for slug in store.preset_slugs() {
        if let Some(preset) = store.preset(&slug) {
            for stage in &preset.stages {
                if stage.kind == "normalize" {
                    if let Some(v) = stage.params.get("dpi") {
                        if let Some(v) = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
                            return v;
                        }
                    }
                }
            }
        }
    }
    1000.0
}
