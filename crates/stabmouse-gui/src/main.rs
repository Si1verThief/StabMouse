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
mod history;
mod keys;
mod presets;
mod profiles;

slint::include_modules!();

use slint::{ModelRc, SharedString, VecModel};
use stabmouse_ipc::client::Client;
use std::cell::RefCell;
use std::rc::Rc;

/// Everything the dashboard shows, gathered in one place.
///
/// Read in a single pass so the window can never display a half-updated mixture — a mode from
/// before a switch beside a pipeline from after it is worse than being briefly stale.
#[derive(Default)]
struct View {
    connected: bool,
    profile: String,
    profile_slug: String,
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
        profile_slug: text(&status, "profile_slug"),
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
    app.set_active_profile_slug(view.profile_slug.into());
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

/// Push the preset list and the selected preset's editable rows into the window.
///
/// Rows come from the **catalog** merged with the file, not from the file alone: a stage just
/// added has no parameters written yet, and an editor that showed only what was already there
/// could never fill one in. The file supplies values; the catalog supplies what exists.
fn apply_presets(app: &App, selected: usize) {
    use stabmouse_config::ParamKind;

    let files = presets::load_all();
    // Which presets the *running* profile loads. Editing one it does not is a silent no-op,
    // and finding that out by tuning an unloaded preset for an evening is a poor way to learn.
    let live: Vec<String> = Client::connect()
        .and_then(|c| c.modes())
        .map(|modes| modes.into_iter().map(|m| m.preset).collect())
        .unwrap_or_default();
    let rows: Vec<PresetRow> = files
        .iter()
        .map(|f| PresetRow {
            slug: f.slug.clone().into(),
            name: f.display_name.clone().into(),
            live: live.iter().any(|p| *p == f.slug),
        })
        .collect();

    let selected = selected.min(files.len().saturating_sub(1));
    let mut out: Vec<EditorRow> = Vec::new();

    if let Some(file) = files.get(selected) {
        let count = file.stages.len();
        for (index, stage) in file.stages.iter().enumerate() {
            let spec = stabmouse_config::catalog::stage(&stage.kind);
            let pinned_first = spec.is_some_and(|s| s.pinned_first);
            let pinned_last = spec.is_some_and(|s| s.pinned_last);

            out.push(EditorRow {
                is_header: true,
                stage_index: index as i32,
                stage_kind: stage.kind.clone().into(),
                stage_label: spec.map(|s| s.label).unwrap_or(&stage.kind).into(),
                stage_enabled: stage.enabled,
                // The assembler re-pins these however they are ordered, so an arrow that
                // moved them would silently undo itself.
                can_up: index > 0 && !pinned_first && !pinned_last,
                can_down: index + 1 < count && !pinned_first && !pinned_last,
                ..Default::default()
            });

            // Catalog order, so a stage always presents its knobs the same way round — and
            // anything the file carries that the catalog does not know about after it, rather
            // than dropped.
            // What the stage's own parameters currently say, so a dependent one can be judged
            // against them rather than shown regardless.
            let value_of = |key: &str| -> Option<String> {
                stage
                    .params
                    .iter()
                    .find(|q| q.key == key)
                    .map(|q| q.text.clone())
                    .or_else(|| {
                        stabmouse_config::catalog::param(&stage.kind, key).map(|p| match p.kind {
                            ParamKind::Choice { default, .. } => default.to_string(),
                            ParamKind::Bool { default } => default.to_string(),
                            _ => String::new(),
                        })
                    })
            };

            let mut seen: Vec<&str> = Vec::new();
            if let Some(spec) = spec {
                for p in spec.params {
                    seen.push(p.key);
                    // **A control that cannot do anything is not shown.** The joystick's
                    // settings under a drag gesture, the curve's under a flat sensitivity —
                    // leaving them visible implies they are doing something.
                    if let Some((on, wanted)) = p.depends_on {
                        if value_of(on).as_deref() != Some(wanted) {
                            continue;
                        }
                    }
                    // A halfway state needs more than one part to be halfway through, so a
                    // setting that depends on one is offered only where it can act.
                    if let Some(binding) = p.needs_chord {
                        let chorded = stage
                            .params
                            .iter()
                            .find(|q| q.key == binding)
                            .map(|q| presets::binding_names(&q.raw))
                            .unwrap_or_default()
                            .iter()
                            .any(|entry| entry.contains('+'));
                        if !chorded {
                            continue;
                        }
                    }
                    let stored = stage.params.iter().find(|q| q.key == p.key);
                    let mut row = EditorRow {
                        stage_index: index as i32,
                        stage_kind: stage.kind.clone().into(),
                        key: p.key.into(),
                        label: p.label.into(),
                        unit: p.unit.into(),
                        help: p.help.into(),
                        ..Default::default()
                    };
                    match p.kind {
                        ParamKind::Float { default, soft_min, soft_max, decimals } => {
                            let value = stored.map(|s| s.value).unwrap_or(default);
                            let lo = soft_min.min(value);
                            let hi = if soft_max > value { soft_max } else { value };
                            row.kind = "float".into();
                            row.value = value as f32;
                            row.minimum = lo as f32;
                            row.maximum = if hi > lo { hi as f32 } else { (lo + 1.0) as f32 };
                            row.display =
                                format!("{:.*}", decimals.min(6) as usize, value).into();
                            row.round_factor = 10f32.powi(i32::from(decimals.min(6)));
                        }
                        ParamKind::Bool { default } => {
                            row.kind = "bool".into();
                            row.flag = stored
                                .map(|s| s.text == "true")
                                .unwrap_or(default);
                        }
                        ParamKind::Choice { default, options } => {
                            row.kind = "choice".into();
                            row.choice = stored
                                .map(|s| s.text.clone())
                                .unwrap_or_else(|| default.to_string())
                                .into();
                            row.options = ModelRc::from(Rc::new(VecModel::from(
                                options
                                    .iter()
                                    .map(|o| SharedString::from(*o))
                                    .collect::<Vec<_>>(),
                            )));
                        }
                        ParamKind::Binding => {
                            row.kind = "binding".into();
                            // Reuses `options` as the list of bound names: a binding is now
                            // several, and the chip control shows each with its own remove.
                            let names = stored
                                .map(|s| presets::binding_names(&s.raw))
                                .unwrap_or_default();
                            row.options = ModelRc::from(Rc::new(VecModel::from(
                                names
                                    .iter()
                                    .map(|n| SharedString::from(n.clone()))
                                    .collect::<Vec<_>>(),
                            )));
                        }
                    }
                    out.push(row);
                }
            }

            // A key the file has and the catalog does not — an older schema, or a hand-written
            // experiment. Shown read-only rather than hidden: silently omitting it is exactly
            // what the format-preserving config exists to prevent.
            for param in &stage.params {
                if seen.contains(&param.key.as_str()) {
                    continue;
                }
                out.push(EditorRow {
                    stage_index: index as i32,
                    stage_kind: stage.kind.clone().into(),
                    key: param.key.clone().into(),
                    label: presets::humanise(&param.key).into(),
                    kind: "binding".into(),
                    choice: param.text.clone().into(),
                    ..Default::default()
                });
            }
        }
        app.set_selected_preset_path(file.path.display().to_string().into());
    } else {
        app.set_selected_preset_path(Default::default());
    }

    app.set_presets(ModelRc::from(Rc::new(VecModel::from(rows))));
    app.set_params(ModelRc::from(Rc::new(VecModel::from(out))));
    app.set_selected_preset(selected as i32);
}

fn apply_profiles(app: &App, selected: usize) {
    let files = profiles::load_all();
    let rows: Vec<ProfileRow> = files
        .iter()
        .map(|f| ProfileRow {
            slug: f.slug.clone().into(),
            name: f.display_name.clone().into(),
        })
        .collect();

    let selected = selected.min(files.len().saturating_sub(1));
    let mut slots: Vec<SlotRow> = Vec::new();
    if let Some(file) = files.get(selected) {
        let count = file.modes.len();
        for (index, slot) in file.modes.iter().enumerate() {
            slots.push(SlotRow {
                index: index as i32,
                name: slot.name.clone().into(),
                output: slot.output.clone().into(),
                preset: slot.preset.clone().into(),
                // Slots are 1-based everywhere the user can see them.
                is_default: file.default_mode == index + 1,
                can_up: index > 0,
                can_down: index + 1 < count,
            });
        }
        app.set_selected_profile_path(file.path.display().to_string().into());
    } else {
        app.set_selected_profile_path(Default::default());
    }

    app.set_startup_profile_slug(profiles::default_slug().unwrap_or_default().into());
    app.set_profiles(ModelRc::from(Rc::new(VecModel::from(rows))));
    app.set_slots(ModelRc::from(Rc::new(VecModel::from(slots))));
    app.set_selected_profile(selected as i32);

    // The presets a slot may point at, so the choice is never a free-text guess at a slug.
    let names: Vec<SharedString> = presets::load_all()
        .iter()
        .map(|p| SharedString::from(p.slug.clone()))
        .collect();
    app.set_preset_slugs(ModelRc::from(Rc::new(VecModel::from(names))));
}

/// Report what went wrong where the user is looking, rather than only on a terminal they may
/// never see. Cleared by the next successful action.
fn notice(app: &App, message: impl std::fmt::Display) {
    let text = message.to_string();
    eprintln!("{text}");
    app.set_notice(text.into());
}

fn selected_preset_path(app: &App) -> Option<std::path::PathBuf> {
    presets::load_all()
        .get(app.get_selected_preset().max(0) as usize)
        .map(|f| f.path.clone())
}

fn selected_profile_path(app: &App) -> Option<std::path::PathBuf> {
    profiles::load_all()
        .get(app.get_selected_profile().max(0) as usize)
        .map(|f| f.path.clone())
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

    apply_presets(&app, 0);
    apply_profiles(&app, 0);
    app.set_outputs(ModelRc::from(Rc::new(VecModel::from(
        profiles::OUTPUTS
            .iter()
            .map(|o| SharedString::from(*o))
            .collect::<Vec<_>>(),
    ))));
    app.set_stage_kinds(ModelRc::from(Rc::new(VecModel::from(
        stabmouse_config::STAGES
            .iter()
            .map(|s| SharedString::from(s.kind))
            .collect::<Vec<_>>(),
    ))));

    // One undo stack for the window, shared by every action that touches a file.
    let history: Rc<RefCell<history::History>> = Rc::new(RefCell::new(history::History::default()));

    // **A Slint timer stops the moment its handle drops.** The first press-to-bind created one
    // inside the handler, so it was destroyed as that function returned and never fired once —
    // the daemon captured the button and reported it correctly, and nothing was ever there to
    // collect it. Owning the handle here keeps it running for the life of the window.
    let capture_timer: Rc<slint::Timer> = Rc::new(slint::Timer::default());

    /// The keyboard half of a chord, built here while the daemon builds the mouse half.
    #[derive(Default)]
    struct KeyChord {
        /// Names held, in press order, so `Ctrl+A` reads the way it was typed.
        parts: Vec<String>,
        /// Non-modifier keys still down. A chord is finished when the hand lets go.
        down: usize,
        /// Set once everything is released, so the poll can allow a moment for a mouse
        /// button to arrive before committing a keyboard-only chord.
        released_at: Option<std::time::Instant>,
    }
    let chord: Rc<RefCell<KeyChord>> = Rc::new(RefCell::new(KeyChord::default()));

    /// Snapshot a file before changing it, so the change can be taken back.
    fn remember(history: &Rc<RefCell<history::History>>, path: &std::path::Path, what: &str) {
        history.borrow_mut().record(path, what);
    }

    fn show_undo(app: &App, history: &Rc<RefCell<history::History>>) {
        let label = history.borrow().next_label().unwrap_or_default().to_string();
        app.set_undo_label(label.into());
    }

    // Every editor action ends the same way: do it, then re-read from disk. Re-reading rather
    // than patching the model in place is what keeps the window showing the file's truth —
    // including any rounding the format-preserving editor applied, and any edit made in a
    // text editor since.
    macro_rules! after {
        ($app:expr, $result:expr, $reload:expr) => {{
            match $result {
                Ok(()) => $app.set_notice(Default::default()),
                Err(e) => notice(&$app, e),
            }
            $reload;
        }};
    }

    let weak = app.as_weak();
    app.on_select_preset(move |index| {
        if let Some(app) = weak.upgrade() {
            apply_presets(&app, index.max(0) as usize);
        }
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_create_preset(move |name| {
        let Some(app) = weak.upgrade() else { return };
        // Recorded before it exists, so undo knows to remove it again.
        remember(
            &hist,
            &presets::preset_path(name.as_str()),
            "create preset",
        );
        match presets::create_preset(name.as_str()) {
            Ok(_) => {
                app.set_notice(Default::default());
                // Land on what was just made, which is what the user is about to fill in.
                let files = presets::load_all();
                let index = files
                    .iter()
                    .position(|f| f.slug == presets::slugify(name.as_str()))
                    .unwrap_or(0);
                apply_presets(&app, index);
                apply_profiles(&app, app.get_selected_profile().max(0) as usize);
            }
            Err(e) => notice(&app, e),
        }
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_delete_preset(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        let slug = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        // Reference integrity is a stated rule (modules.md): never silently break a profile.
        let used_by = presets::profiles_using(&slug);
        if !used_by.is_empty() {
            notice(
                &app,
                format!(
                    "'{slug}' is still used by {}. Point those slots elsewhere first.",
                    used_by.join(", ")
                ),
            );
            return;
        }
        // Recorded with the file's whole contents, so undo brings it back rather than
        // leaving the user with a name and no pipeline.
        remember(&hist, &path, "delete preset");
        after!(app, presets::delete_preset(&path), apply_presets(&app, 0));
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_add_stage(move |kind| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else {
            notice(&app, "create a preset first");
            return;
        };
        remember(&hist, &path, "add stage");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(app, presets::add_stage(&path, kind.as_str()), apply_presets(&app, selected));
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_remove_stage(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "remove stage");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::remove_stage(&path, index.max(0) as usize),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_move_stage(move |index, delta| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "reorder stage");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::move_stage(&path, index.max(0) as usize, delta),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_stage_enabled(move |index, enabled| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "toggle stage");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::write_stage_enabled(&path, index.max(0) as usize, enabled),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_param_float(move |index, key, value| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "change value");
        // **Written, but the model is not rebuilt.** A drag emits a change per frame, and
        // re-reading on each one replaced the model under the hand — the control now owns the
        // value while it is being dragged, so there is nothing here worth interrupting it for.
        // The file is still the truth, and the next selection or action reads it back.
        match presets::write_param(&path, index.max(0) as usize, key.as_str(), f64::from(value)) {
            Ok(()) => app.set_notice(Default::default()),
            Err(e) => notice(&app, e),
        }
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_param_bool(move |index, key, value| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "change setting");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::write_param_text(
                &path,
                index.max(0) as usize,
                key.as_str(),
                toml::Value::Boolean(value)
            ),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_param_text(move |index, key, value| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "change setting");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::write_param_text(
                &path,
                index.max(0) as usize,
                key.as_str(),
                toml::Value::String(value.to_string())
            ),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    // ---------------------------------------------------------------- profiles

    let weak = app.as_weak();
    app.on_select_profile(move |index| {
        if let Some(app) = weak.upgrade() {
            apply_profiles(&app, index.max(0) as usize);
        }
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_create_profile(move |name| {
        let Some(app) = weak.upgrade() else { return };
        remember(&hist, &profiles::profile_path(name.as_str()), "create profile");
        match profiles::create(name.as_str()) {
            Ok(_) => {
                app.set_notice(Default::default());
                let files = profiles::load_all();
                let index = files
                    .iter()
                    .position(|f| f.slug == presets::slugify(name.as_str()))
                    .unwrap_or(0);
                apply_profiles(&app, index);
            }
            Err(e) => notice(&app, e),
        }
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_delete_profile(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else { return };
        remember(&hist, &path, "delete profile");
        after!(app, profiles::delete(&path), apply_profiles(&app, 0));
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_add_slot(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else {
            notice(&app, "create a profile first");
            return;
        };
        remember(&hist, &path, "add slot");
        // Points at whatever preset exists, so a new slot is valid the moment it appears
        // rather than referring to a name that is not there.
        let preset = presets::load_all()
            .first()
            .map(|p| p.slug.clone())
            .unwrap_or_else(|| "raw".to_string());
        let selected = app.get_selected_profile().max(0) as usize;
        after!(
            app,
            profiles::add_mode(&path, "New mode", "mouse", &preset),
            apply_profiles(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_remove_slot(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else { return };
        remember(&hist, &path, "remove slot");
        let selected = app.get_selected_profile().max(0) as usize;
        after!(
            app,
            profiles::remove_mode(&path, index.max(0) as usize),
            apply_profiles(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_move_slot(move |index, delta| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else { return };
        remember(&hist, &path, "reorder slot");
        let selected = app.get_selected_profile().max(0) as usize;
        after!(
            app,
            profiles::move_mode(&path, index.max(0) as usize, delta),
            apply_profiles(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_slot_field(move |index, field, value| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else { return };
        remember(&hist, &path, "change slot");
        let selected = app.get_selected_profile().max(0) as usize;
        after!(
            app,
            profiles::set_mode_field(&path, index.max(0) as usize, field.as_str(), value.as_str()),
            apply_profiles(&app, selected)
        );
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_set_default_slot(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_profile_path(&app) else { return };
        remember(&hist, &path, "set default slot");
        let selected = app.get_selected_profile().max(0) as usize;
        after!(
            app,
            profiles::set_default_mode(&path, index.max(0) as usize + 1),
            apply_profiles(&app, selected)
        );
        show_undo(&app, &hist);
    });

    // ------------------------------------------------------------ press-to-bind
    //
    // The daemon watches for the button, because it holds the grab and we cannot see it. This
    // side polls for the answer and stops on the first of: a capture, a cancel, or a timeout
    // matching the daemon's own window — a control that waits forever is one the user cannot
    // tell from a broken one.
    // Keys arrive here; the daemon reports buttons. Neither can see the other's, which is why
    // both halves exist — and why a chord is only complete once both have had their say.
    let chord_for_down = chord.clone();
    app.on_key_down(move |text, ctrl, alt, shift, meta| {
        let mut chord = chord_for_down.borrow_mut();
        for name in keys::modifier_names(ctrl, alt, shift, meta) {
            if !chord.parts.contains(&name) {
                chord.parts.push(name);
            }
        }
        if let Some(name) = keys::evdev_name(text.as_str()) {
            if !keys::is_modifier(&name) {
                chord.down += 1;
                if !chord.parts.contains(&name) {
                    chord.parts.push(name);
                }
            }
        }
        chord.released_at = None;
    });

    let chord_for_up = chord.clone();
    app.on_key_up(move |text| {
        let mut chord = chord_for_up.borrow_mut();
        if let Some(name) = keys::evdev_name(text.as_str()) {
            if !keys::is_modifier(&name) {
                chord.down = chord.down.saturating_sub(1);
            }
        }
        if chord.down == 0 && !chord.parts.is_empty() {
            chord.released_at = Some(std::time::Instant::now());
        }
    });

    let weak = app.as_weak();
    let hist = history.clone();
    let timer = capture_timer.clone();
    let chord_for_listen = chord.clone();
    app.on_listen_binding(move |stage_index, key| {
        let Some(app) = weak.upgrade() else { return };
        *chord_for_listen.borrow_mut() = KeyChord::default();
        if Client::connect().and_then(|c| c.capture_binding()).is_err() {
            notice(&app, "no daemon is running, so it cannot watch for a button");
            return;
        }
        app.set_listening_for(format!("{stage_index}/{key}").into());

        let weak = app.as_weak();
        let hist = hist.clone();
        let chord = chord_for_listen.clone();
        let key = key.to_string();
        let mut waited = std::time::Duration::ZERO;
        let poll = std::time::Duration::from_millis(80);
        // The daemon gives up after 8s; matching that keeps the two ends agreeing about
        // whether a capture is still live.
        let limit = std::time::Duration::from_secs(8);
        timer.start(
            slint::TimerMode::Repeated,
            poll,
            move || {
                let Some(app) = weak.upgrade() else { return };
                // Cancelled from elsewhere — Escape, or the button.
                if app.get_listening_for().is_empty() {
                    return;
                }
                waited += poll;
                let buttons = Client::connect()
                    .and_then(|c| c.take_captured_binding())
                    .unwrap_or_default();

                // A chord is done when the daemon has published its buttons, or when the keys
                // have been released long enough that no button is coming. The grace is what
                // lets `Ctrl+A+Middle` arrive as one binding rather than as two races.
                const GRACE: std::time::Duration = std::time::Duration::from_millis(350);
                let keys_settled = chord
                    .borrow()
                    .released_at
                    .is_some_and(|t| t.elapsed() >= GRACE);

                let ready = !buttons.is_empty() || keys_settled;
                if !ready {
                    if waited >= limit {
                        app.set_listening_for(Default::default());
                        let _ = Client::connect().and_then(|c| c.cancel_binding_capture());
                        notice(&app, "nothing was pressed, so nothing was bound");
                    }
                    return;
                }

                // Keys first, then buttons, so a chord reads the way a person writes one.
                let mut parts = chord.borrow().parts.clone();
                if !buttons.is_empty() {
                    parts.extend(buttons.split('+').map(str::to_string));
                }
                *chord.borrow_mut() = KeyChord::default();
                app.set_listening_for(Default::default());
                // The daemon is still watching unless told otherwise, and while it watches it
                // swallows every mouse event — which reads as the mouse being stuck in
                // binding mode after a keyboard-only chord.
                if buttons.is_empty() {
                    let _ = Client::connect().and_then(|c| c.cancel_binding_capture());
                }
                if parts.is_empty() {
                    notice(&app, "nothing was pressed, so nothing was bound");
                    return;
                }
                let combined = parts.join("+");
                if let Some(path) = selected_preset_path(&app) {
                    remember(&hist, &path, "add binding");
                    let selected = app.get_selected_preset().max(0) as usize;
                    after!(
                        app,
                        presets::add_binding(
                            &path,
                            stage_index.max(0) as usize,
                            &key,
                            &combined
                        ),
                        apply_presets(&app, selected)
                    );
                    show_undo(&app, &hist);
                }
            },
        );
    });

    let weak = app.as_weak();
    let chord_for_cancel = chord.clone();
    app.on_cancel_listen(move || {
        if let Some(app) = weak.upgrade() {
            app.set_listening_for(Default::default());
            app.set_notice(Default::default());
            *chord_for_cancel.borrow_mut() = KeyChord::default();
            let _ = Client::connect().and_then(|c| c.cancel_binding_capture());
        }
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_remove_binding(move |stage_index, key, name| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = selected_preset_path(&app) else { return };
        remember(&hist, &path, "remove binding");
        let selected = app.get_selected_preset().max(0) as usize;
        after!(
            app,
            presets::remove_binding(&path, stage_index.max(0) as usize, key.as_str(), name.as_str()),
            apply_presets(&app, selected)
        );
        show_undo(&app, &hist);
    });

    // Switching profile is a daemon action rather than a file edit, so it goes over the bus
    // and the dashboard's own refresh reports the result.
    let weak = app.as_weak();
    app.on_switch_profile(move |slug| {
        if let Some(app) = weak.upgrade() {
            act(&app, |c| c.set_profile(slug.as_str()));
        }
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_make_startup_profile(move |slug| {
        let Some(app) = weak.upgrade() else { return };
        let path = presets::config_dir().join("config.toml");
        remember(&hist, &path, "set startup profile");
        match profiles::set_default(slug.as_str()) {
            Ok(()) => app.set_notice(Default::default()),
            Err(e) => notice(&app, e),
        }
        apply_profiles(&app, app.get_selected_profile().max(0) as usize);
        show_undo(&app, &hist);
    });

    let weak = app.as_weak();
    let hist = history.clone();
    app.on_undo(move || {
        let Some(app) = weak.upgrade() else { return };
        match hist.borrow_mut().undo() {
            Ok(what) => app.set_notice(format!("undid: {what}").into()),
            Err(e) => notice(&app, e),
        }
        // Both editors re-read: an undo may have restored a preset a profile refers to, or a
        // profile whose slots name presets, and showing one stale would be its own bug.
        apply_presets(&app, app.get_selected_preset().max(0) as usize);
        apply_profiles(&app, app.get_selected_profile().max(0) as usize);
        show_undo(&app, &hist);
    });

    app.run()
}
