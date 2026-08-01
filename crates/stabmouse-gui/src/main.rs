//! StabMouse — the graphical frontend.
//!
//! A separate process from the daemon, per D4: idle cost is zero because this is not running,
//! the toolkit choice never touches the hot path, and a Flatpak frontend talking to a native
//! daemon stays possible later.
//!
//! # It opens whether or not a daemon is running
//!
//! Refusing to start without one would be the wrong failure. "No daemon" is a normal state
//! with an obvious remedy, and a window that explains it is more useful than a terminal error
//! the user may never see — this is the frontend for people who are not in a terminal.

mod desktop;

slint::include_modules!();

use slint::{ModelRc, SharedString, VecModel};
use stabmouse_ipc::client::Client;
use std::rc::Rc;

/// Everything the dashboard shows, gathered in one place.
///
/// Read in a single pass so the window can never display a half-updated mixture — a mode from
/// before a switch beside a pipeline from after it is worse than being briefly stale.
#[derive(Default)]
struct View {
    connected: bool,
    profile: String,
    slot: i32,
    mode_name: String,
    enabled: bool,
    degraded: bool,
    degraded_reason: String,
    tablets: i32,
    tablets_placed: bool,
    version: String,
    modes: Vec<ModeRow>,
    stages: Vec<SharedString>,
}

fn text(map: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> String {
    map.get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn flag(map: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> bool {
    map.get(key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn number(map: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> i32 {
    map.get(key)
        .and_then(|v| u32::try_from(v.clone()).ok())
        .unwrap_or(0) as i32
}

fn gather() -> View {
    let Ok(client) = Client::connect() else {
        return View::default();
    };
    let Ok(status) = client.status() else {
        return View::default();
    };

    let modes = client.modes().unwrap_or_default();
    let slot = number(&status, "mode_slot");

    // The pipeline strip shows the *active* mode's stages, which is the answer to "is it doing
    // anything" — the first question a user has and the hardest to answer any other way.
    //
    // Only the preset name is on the wire today, so that is what is shown rather than a stage
    // list invented here. An honest single chip beats a plausible fabricated strip.
    let stages = modes
        .iter()
        .find(|m| m.slot as i32 == slot)
        .map(|m| {
            vec![
                SharedString::from(m.output.clone()),
                SharedString::from(format!("preset: {}", m.preset)),
            ]
        })
        .unwrap_or_default();

    View {
        connected: true,
        profile: text(&status, "profile"),
        slot,
        mode_name: text(&status, "mode_name"),
        enabled: flag(&status, "enabled"),
        degraded: flag(&status, "degraded"),
        degraded_reason: text(&status, "degraded_reason"),
        tablets: number(&status, "tablets"),
        tablets_placed: flag(&status, "tablets_placed"),
        version: text(&status, "version"),
        modes: modes
            .into_iter()
            .map(|m| ModeRow {
                slot: m.slot as i32,
                name: m.name.into(),
                output: m.output.into(),
                preset: m.preset.into(),
            })
            .collect(),
        stages,
    }
}

fn apply(app: &App, view: View) {
    app.set_connected(view.connected);
    app.set_profile(view.profile.into());
    app.set_active_slot(view.slot);
    app.set_mode_name(view.mode_name.into());
    app.set_enabled(view.enabled);
    app.set_degraded(view.degraded);
    app.set_degraded_reason(view.degraded_reason.into());
    app.set_tablets(view.tablets);
    app.set_tablets_placed(view.tablets_placed);
    app.set_version(view.version.into());
    app.set_modes(ModelRc::from(Rc::new(VecModel::from(view.modes))));
    app.set_stages(ModelRc::from(Rc::new(VecModel::from(view.stages))));
}

/// Ask the daemon to do something.
///
/// **Deliberately does not re-read afterwards.** Commands are one-way, so reading immediately
/// after sending one observes the state from *before* it — the daemon has not processed it
/// yet. That was the whole of the enable/disable bug: the button appeared to need several
/// clicks because each click was displaying the result of the previous one.
///
/// The daemon announces every state change, and [`watch_daemon`] re-reads on the announcement,
/// so the value shown is always the value after. The only case handled here is the call
/// failing outright, which means the daemon is gone and no announcement is coming.
fn act(app: &App, action: impl FnOnce(&Client) -> stabmouse_ipc::Result<()>) {
    match Client::connect() {
        Ok(client) => {
            if action(&client).is_err() {
                apply(app, gather());
            }
        }
        Err(_) => apply(app, gather()),
    }
}

/// Re-read whenever the daemon announces a change, for as long as the window lives.
///
/// Covers far more than this window's own actions: a hotkey, the CLI, a second window, or a
/// switch that was deferred until the end of a stroke. A frontend that only refreshes after
/// its own commands is wrong the moment anything else touches the daemon.
///
/// Reconnects rather than giving up, so starting the daemon after the window is already open
/// works without the user having to press anything.
fn watch_daemon(weak: slint::Weak<App>) {
    std::thread::spawn(move || loop {
        let weak_for_signal = weak.clone();
        let _ = stabmouse_ipc::client::on_change(move || {
            let weak = weak_for_signal.clone();
            // The UI may only be touched from the event loop; this hands the work over.
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    apply(&app, gather());
                }
            });
        });

        // The subscription ended, which means the bus or the daemon went away. Show that, then
        // retry — slowly, because a missing daemon is a normal state and not worth spinning on.
        let weak_for_drop = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak_for_drop.upgrade() {
                apply(&app, gather());
            }
        });
        std::thread::sleep(std::time::Duration::from_secs(2));
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let app = App::new()?;

    let (r, g, b) = desktop::accent();
    app.global::<Theme>()
        .set_accent(slint::Color::from_rgb_u8(r, g, b));
    app.global::<Theme>().set_dark(desktop::prefers_dark());

    apply(&app, gather());
    watch_daemon(app.as_weak());

    let weak = app.as_weak();
    app.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            apply(&app, gather());
        }
    });

    let weak = app.as_weak();
    app.on_switch_to(move |slot| {
        if let Some(app) = weak.upgrade() {
            act(&app, |c| c.set_mode(slot.max(1) as u32));
        }
    });

    let weak = app.as_weak();
    app.on_set_enabled(move |enabled| {
        if let Some(app) = weak.upgrade() {
            act(&app, |c| c.set_enabled(enabled));
        }
    });

    app.run()
}
