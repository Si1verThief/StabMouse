//! The feel bench.
//!
//! Records real strokes, then replays that *identical* input through several
//! candidate configurations and emits comparable output.
//!
//! This exists because comparing filters by drawing a fresh line each time is not a
//! comparison — you cannot tell whether one setting beat another or whether your hand
//! was steadier that attempt. Every filter finding on this project so far came from
//! use rather than from reasoning, and this is the instrument that makes such findings
//! attributable.

mod recording;
mod variant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use evdev::{Device, EventType, RelativeAxisCode, KeyCode};
use recording::Recording;
use stabmouse_core::{Quantizer, Sample};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "stabmouse-replay", about = "Record and replay strokes for filter research")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Capture motion from a real mouse. Does not grab, so use the mouse normally.
    Record {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Device resolution, recorded in the header so replays are physically correct.
        #[arg(long, default_value_t = 1600.0)]
        dpi: f64,
        /// Stop after this many seconds. Omit to run until Ctrl-C.
        #[arg(long)]
        seconds: Option<u64>,
    },
    /// Describe a recording without replaying it.
    Info {
        #[arg(long)]
        input: PathBuf,
    },
    /// Replay one recording through every variant and write a comparable CSV.
    Compare {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        variants: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Maximum interval between samples. Gaps longer than this are filled with
        /// zero-motion ticks, mirroring the daemon: filters cannot evolve without
        /// samples, so a 60ms mouse gap would otherwise skip a whole attack envelope.
        /// 0 disables, replaying only recorded events.
        #[arg(long, default_value_t = 4)]
        tick_ms: u64,
    },
    /// Write a starting variants file.
    ExampleVariants {
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Record {
            source,
            out,
            dpi,
            seconds,
        } => record(&source, &out, dpi, seconds),
        Command::Info { input } => {
            println!("{}", Recording::load(&input)?.summary());
            Ok(())
        }
        Command::Compare {
            input,
            variants,
            out,
            tick_ms,
        } => compare(&input, &variants, &out, tick_ms),
        Command::ExampleVariants { out } => {
            std::fs::write(&out, variant::EXAMPLE)
                .with_context(|| format!("writing {}", out.display()))?;
            println!("wrote {}", out.display());
            Ok(())
        }
    }
}

fn record(source: &PathBuf, out: &PathBuf, dpi: f64, seconds: Option<u64>) -> Result<()> {
    let mut device =
        Device::open(source).with_context(|| format!("opening {}", source.display()))?;
    let name = device.name().unwrap_or("unknown").to_owned();

    let mut file = std::fs::File::create(out)
        .with_context(|| format!("creating {}", out.display()))?;
    file.write_all(Recording::header(&name, dpi).as_bytes())?;

    println!("recording {name} -> {}", out.display());
    println!("The device is NOT grabbed; use the mouse normally. Hold left button to");
    println!("mark a stroke. Ctrl-C to stop.");

    let deadline = seconds.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));

    let mut dx = 0i32;
    let mut dy = 0i32;
    let mut down = false;
    let mut t_us = 0u64;
    let mut written = 0u64;

    'outer: loop {
        for event in device.fetch_events().context("reading source")? {
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
                        down = event.value() != 0;
                    }
                }
                EventType::SYNCHRONIZATION => {
                    // Timestamps come from the kernel event, not from a clock read
                    // here, so replay reproduces the original intervals.
                    t_us = event
                        .timestamp()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_micros() as u64)
                        .unwrap_or(t_us + 1_000);

                    // Flush per line: Ctrl-C will not run any cleanup, and a partial
                    // recording is more useful than a truncated buffer.
                    writeln!(file, "{t_us}\t{dx}\t{dy}\t{}", u8::from(down))?;
                    file.flush()?;
                    written += 1;

                    dx = 0;
                    dy = 0;
                }
                _ => {}
            }
        }

        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                break 'outer;
            }
        }
    }

    println!("wrote {written} samples to {}", out.display());
    Ok(())
}

/// One sample to feed the pipeline: recorded, or a synthesised tick.
struct Step {
    t_us: u64,
    dx: f64,
    dy: f64,
    down: bool,
    tick: bool,
}

/// Fill gaps with zero-motion ticks.
///
/// The daemon must do this (see modules.md) because filters are driven by samples, not
/// by wall time. Without it a 60ms gap in the recording advances an attack envelope by a
/// full 60ms in one step, and a stabiliser never closes its lag after a stroke.
fn expand(events: &[recording::Event], tick_us: u64) -> Vec<Step> {
    let mut out = Vec::with_capacity(events.len());
    let mut last: Option<u64> = None;
    let mut down = false;

    for e in events {
        if tick_us > 0 {
            if let Some(prev) = last {
                // Stop half a tick short of the next real event. Otherwise a tick can
                // land microseconds before it, and any rate divided by that interval
                // explodes -- it produced 600,000 mm/s spikes in the plots.
                let guard = e.t_us.saturating_sub(tick_us / 2);
                let mut t = prev.saturating_add(tick_us);
                while t < guard {
                    out.push(Step { t_us: t, dx: 0.0, dy: 0.0, down, tick: true });
                    t = t.saturating_add(tick_us);
                }
            }
        }
        out.push(Step {
            t_us: e.t_us,
            dx: f64::from(e.dx),
            dy: f64::from(e.dy),
            down: e.down,
            tick: false,
        });
        down = e.down;
        last = Some(e.t_us);
    }
    out
}

fn compare(input: &PathBuf, variants: &PathBuf, out: &PathBuf, tick_ms: u64) -> Result<()> {
    let rec = Recording::load(input)?;
    let variants = variant::load(variants)?;
    let tick_us = tick_ms.saturating_mul(1_000);
    let steps = expand(&rec.events, tick_us);

    eprintln!("{}", rec.summary());
    if tick_us > 0 {
        eprintln!(
            "{} recorded samples expanded to {} with {}ms ticks",
            rec.events.len(),
            steps.len(),
            tick_ms
        );
    }
    eprintln!();

    let mut file = std::fs::File::create(out)
        .with_context(|| format!("creating {}", out.display()))?;
    writeln!(
        file,
        "variant,i,t_us,dt_ms,in_dx,in_dy,out_dx_mm,out_dy_mm,out_x_mm,out_y_mm,out_dx_counts,speed_mm_s,pressure,down"
    )?;

    let (in_x, in_y) = rec.total_counts();

    for v in &variants {
        let mut pipeline = v.build(rec.dpi);
        let mut quantizer = Quantizer::new(rec.dpi);

        let mut x_mm = 0.0f64;
        let mut y_mm = 0.0f64;
        let mut out_counts_x = 0i64;
        let mut out_counts_y = 0i64;
        let mut pressure_sum = 0.0;
        let mut pressure_max = 0.0f64;
        let mut pressure_n = 0u64;
        // Largest single-sample pressure jump. This is what a blob looks like
        // numerically, so it measures the artefact directly rather than by proxy.
        let mut worst_jump = 0.0f64;
        let mut prev_pressure = 0.0f64;

        let mut samples_into_stroke = 0u32;

        for (i, e) in steps.iter().enumerate() {
            let mut s = Sample::new(e.dx, e.dy, e.t_us, e.down);
            pipeline.process(&mut s);

            x_mm += s.dx;
            y_mm += s.dy;
            let (cx, cy) = quantizer.quantize(s.dx, s.dy);
            out_counts_x += i64::from(cx);
            out_counts_y += i64::from(cy);

            let p = s.pressure.unwrap_or(0.0);
            if e.down {
                samples_into_stroke += 1;
                pressure_sum += p;
                pressure_max = pressure_max.max(p);
                // Skip stroke onsets: pressure legitimately climbs from zero there, and
                // including it would swamp the mid-stroke artefacts this measures.
                if samples_into_stroke > 3 {
                    worst_jump = worst_jump.max((p - prev_pressure).abs());
                }
                pressure_n += 1;
            } else {
                samples_into_stroke = 0;
            }
            prev_pressure = p;

            writeln!(
                file,
                "{},{},{},{:.3},{},{},{:.6},{:.6},{:.6},{:.6},{},{:.3},{:.6},{}",
                v.name,
                i,
                e.t_us,
                s.dt * 1000.0,
                e.dx,
                e.dy,
                s.dx,
                s.dy,
                x_mm,
                y_mm,
                cx,
                s.speed_mm_s.unwrap_or(0.0),
                p,
                u8::from(e.down)
            )?;
        }

        // Tick to convergence so outstanding lag is flushed and the residual reflects
        // physics rather than un-emitted state.
        let mut t = steps.last().map(|s| s.t_us).unwrap_or(0);
        let settle_tick = if tick_us > 0 { tick_us } else { 4_000 };
        let mut settle_ticks = 0;
        while !pipeline.settled() && settle_ticks < 100_000 {
            t = t.saturating_add(settle_tick);
            let mut s = Sample::new(0.0, 0.0, t, false);
            pipeline.process(&mut s);
            let (cx, cy) = quantizer.quantize(s.dx, s.dy);
            out_counts_x += i64::from(cx);
            out_counts_y += i64::from(cy);
            settle_ticks += 1;
        }

        // Conservation check.
        //
        // A stabiliser rests up to `radius` behind the cursor, and that lag is
        // legitimate — it is what the filter is for, and it is still outstanding when
        // the input ends. So the invariant is not "residual is zero" but "residual does
        // not exceed the radius". Anything beyond that is motion genuinely lost, which
        // is the failure mode that keeps recurring here.
        let lost_x = in_x - out_counts_x;
        let lost_y = in_y - out_counts_y;
        let counts_per_mm = rec.dpi / 25.4;
        let residual_mm = (lost_x as f64).hypot(lost_y as f64) / counts_per_mm;
        let budget_mm = v.stab_radius_mm + 0.05;
        let verdict = if residual_mm <= budget_mm { "ok" } else { "LOSS" };
        let mean_p = if pressure_n > 0 {
            pressure_sum / pressure_n as f64
        } else {
            0.0
        };

        eprintln!(
            "{:<18} residual {:>6.2}mm of {:>5.2} budget {:<4}  pressure mean {:.3} max {:.3}  worst jump {:.4}",
            v.name, residual_mm, budget_mm, verdict, mean_p, pressure_max, worst_jump
        );
    }

    eprintln!();
    eprintln!("wrote {}", out.display());
    eprintln!("residual   lag still outstanding when the input ended. A stabiliser rests");
    eprintln!("           up to its radius behind the cursor, so that much is legitimate;");
    eprintln!("           exceeding the budget means motion was genuinely LOST.");
    eprintln!("worst jump largest single-sample pressure change. High values are the");
    eprintln!("           numerical signature of a blob.");

    Ok(())
}
