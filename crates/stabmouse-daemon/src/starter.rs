//! Writing a starter config.
//!
//! Exists so the daemon is useful before the GUI: there has to be something on disk to
//! edit. Every value carries a comment saying whether it was measured or guessed, because
//! the person editing this file is going to be tuning it and deserves to know which numbers
//! have evidence behind them.

use anyhow::Context;
use std::path::Path;

pub fn write(dir: &Path, force: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir.join("presets")).context("creating presets/")?;
    std::fs::create_dir_all(dir.join("profiles")).context("creating profiles/")?;

    let files: &[(&str, &str)] = &[
        ("config.toml", CONFIG),
        ("presets/raw.toml", RAW),
        ("presets/inking.toml", INKING),
        ("presets/steady.toml", STEADY),
        ("profiles/default.toml", PROFILE),
    ];

    let mut wrote = 0;
    for (rel, body) in files {
        let path = dir.join(rel);
        if path.exists() && !force {
            println!("keeping existing {}", path.display());
            continue;
        }
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
        wrote += 1;
    }

    if wrote > 0 {
        println!();
        println!("Next: add your mouse. `stabmoused devices` lists candidates, then set the");
        println!("vid/pid in config.toml and `managed = true`.");
    }
    Ok(())
}

const CONFIG: &str = r#"# StabMouse configuration.
#
# Comments and formatting in this file are preserved when the GUI edits it. Hand-editing is
# a supported way to work, not a fallback.
schema = 1

[defaults]
profile = "default"

# Whether an application can receive tablet input, by window class. This corrects the automatic
# detection, which reads the toolkit the application loaded (Qt, GTK, SDL3, or X11 under
# XWayland — all of which take a pen) and sends everything it cannot prove to the pointer.
# The main reason to add an entry: promoting a Chromium-based browser whose build is known to
# support the pen, since that cannot be detected from outside.
#
# `stabmouse-probe focus` prints the classes as they are actually reported. Use those exactly.
#
# [tablet_support]
# vivaldi-stable = true
# "org.kde.krita" = true

# Devices are opt-in: anything not listed here is never touched, which is what keeps
# trackpads and unrelated hardware safe. `stabmoused devices` shows vid:pid.
#
# [[device]]
# match = { vid = "0738", pid = "0c08" }
# label = "my mouse"
# managed = true
# overrides = { "inking.normalize.dpi" = 1600 }
"#;

const RAW: &str = r#"# Does nothing. The fallback, and the one to verify against when checking that StabMouse
# is not shaping your input.
#
# Named `raw` rather than `flat` deliberately: "flat" names a kind of acceleration curve, so
# it would imply the signal is still being shaped.
schema = 1
display_name = "Raw"

[[stage]]
type = "normalize"
dpi = 1600
"#;

const INKING: &str = r#"# Careful line work.
#
# Every value is marked MEASURED (derived from real recordings) or PROPOSED (reasoned, not
# yet feel-tested). See docs/stages.md for the measurements.
schema = 1
display_name = "Inking"

[[stage]]
type = "normalize"
dpi = 1600

# MEASURED: mc5/b0.05 gives 0.30mm lag on deliberate strokes. Beta must be in the 0.05-0.2
# range for these units -- it multiplies velocity, and velocity here is mm/s, so the ~0.007
# seen in one-euro literature does nothing at all.
[[stage]]
type = "smooth"
min_cutoff_hz = 5.0
beta = 0.05
d_cutoff_hz = 1.0

# PROPOSED. Bracketed by measurement: 0.2mm removes visible wobble with all detail intact,
# 0.5mm is smooth with the shape preserved, 1.0mm starts rounding corners, 4.0mm destroys a
# drawing that spans only ~8mm of hand movement.
[[stage]]
type = "stabilize"
radius_mm = 0.4
catch_up = 0.35

# MEASURED: careful drawing peaks at 20-30 mm/s within a stroke, so v_max = 50 with
# gamma = 2.0 gives a 0.45 pressure arch and no clipping. An earlier 400 left pressure
# pinned near maximum and every stroke looked flat.
#
# PROPOSED: attack_ms. Every real stroke is 93ms or longer, so at 60ms they all reach full
# pressure -- whether a quick tap should be a solid dot or a light one is a style choice.
[[stage]]
type = "pressure"
attack_ms = 60.0
release_ms = 60.0
v_max_mm_s = 50.0
gamma = 2.0
source = "cursor"
velocity_smoothing_ms = 40.0
stall_behaviour = "hold"
min_pressure = 0.05
"#;

const STEADY: &str = r#"# Tremor and precision assistance, for general cursor use rather than drawing.
#
# Stronger smoothing than `inking` and no pressure stage: this is about making a pointer
# usable, not about strokes.
schema = 1
display_name = "Steady"

[[stage]]
type = "normalize"
dpi = 1600

# MEASURED: mc2/b0.2 removes 44% of path wobble on a tremor recording for 0.44mm of lag on
# deliberate strokes. The strongest setting that still felt controllable in the sweep.
[[stage]]
type = "smooth"
min_cutoff_hz = 2.0
beta = 0.2
d_cutoff_hz = 1.0

# PROPOSED: heavier than inking, since a steadier cursor is worth more lag here.
[[stage]]
type = "stabilize"
radius_mm = 1.5
catch_up = 0.25
"#;

const PROFILE: &str = r#"# A profile holds mode slots. The mode toggle switches between them instantly; switching
# profile is the deliberate, occasional action.
schema = 1
display_name = "Default"
default_mode = 1

# In tablet mode, also send ordinary mouse buttons alongside the pen. Off, and known broken:
# the click lands where the *pointer* was last, not where the pen is, because the compositor
# tracks those two positions separately. Left here only so the switch exists.
# tablet_emits_mouse_clicks = true

# Destroys the virtual tablet on leaving tablet mode; it comes back in about 50ms.
#
# This was written for Krita, which keeps painting a stale canvas cursor after the pen leaves
# proximity. It does not fix that — Krita stays stuck even with the device gone entirely — so
# there is currently no known reason to turn it on. Left here because the mechanism works and
# some other application may need it.
#
# The cost: anything launched while the tablet is absent gets no pressure until restarted.
# destroy_tablet_on_leave = true

# Outputs: "mouse" is the ordinary pointer, delivered absolutely so switching modes never
# teleports the cursor. "tablet" is a pen, dropping to "mouse" per window when the application
# under it cannot take one. "relative" is raw deltas for games and anything else that locks
# the pointer — use it for a gaming mode if a game misreads absolute motion.

[[mode]]
name = "Mouse"
output = "mouse"
preset = "raw"

[[mode]]
name = "Draw"
output = "tablet"
preset = "inking"

[[mode]]
name = "Steady"
output = "mouse"
preset = "steady"
"#;
