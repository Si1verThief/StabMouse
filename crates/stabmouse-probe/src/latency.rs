//! Round-trip latency probe.
//!
//! Answers the open question in decision D5: userspace `evdev` capture adds a
//! kernel→userspace→kernel round trip that a kernel module (yeetmouse, maccel)
//! avoids. The estimate has been ~0.2–0.5ms; this measures it.
//!
//! Closed loop, entirely synthetic — no real mouse involved, so it is safe to run
//! at any time:
//!
//! ```text
//!   emit on synthetic source ──▶ [grab + read + forward] ──▶ sink ──▶ read back
//!        t0                          the "daemon" path                  t1
//! ```
//!
//! Both devices carry `BTN_TRIGGER_HAPPY1` rather than `REL_X`, deliberately: the
//! syscall and kernel path are identical for any event type, but a relative-motion
//! event would drag the real cursor around for the duration of the run. What this
//! measures is transport cost, which is what the round-trip question is about.

use anyhow::{anyhow, Context, Result};
use clap::Args as ClapArgs;
use evdev::{
    uinput::{VirtualDevice},
    AttributeSet, Device, EventType, InputEvent, KeyCode,
};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Nothing in a normal desktop binds this, so emitting it has no visible effect.
const PROBE_KEY: KeyCode = KeyCode::BTN_TRIGGER_HAPPY1;

#[derive(ClapArgs)]
pub struct Args {
    /// Events to measure.
    #[arg(long, default_value_t = 2000)]
    count: usize,

    /// Gap between emitted events, in microseconds. 1000 = 1kHz.
    #[arg(long, default_value_t = 1000)]
    interval_us: u64,

    /// Discard this many events before recording, to let caches warm.
    #[arg(long, default_value_t = 200)]
    warmup: usize,
}

pub fn run(args: Args) -> Result<()> {
    let mut source = build_device("StabMouse probe latency source")?;
    let mut sink = build_device("StabMouse probe latency sink")?;

    let source_node = first_node(&mut source).context("locating source node")?;
    let sink_node = first_node(&mut sink).context("locating sink node")?;

    println!("source: {}", source_node.display());
    println!("sink:   {}", sink_node.display());
    println!(
        "measuring {} events at {}us intervals ({} warmup)...",
        args.count, args.interval_us, args.warmup
    );
    println!();

    // The forwarding thread stands in for the daemon's hot path: grab the source,
    // read, write straight to the sink.
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let forwarder = {
        let source_node = source_node.clone();
        thread::spawn(move || {
            let mut input = match open_with_retry(&source_node) {
                Ok(d) => d,
                Err(e) => {
                    ready_tx.send(Err(e.to_string())).ok();
                    return;
                }
            };
            if let Err(e) = input.grab() {
                ready_tx.send(Err(format!("grab source: {e}"))).ok();
                return;
            }
            ready_tx.send(Ok(())).ok();

            loop {
                let events = match input.fetch_events() {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("forwarder read error: {e}");
                        return;
                    }
                };
                for event in events {
                    if event.event_type() == EventType::KEY {
                        if let Err(e) = sink.emit(&[InputEvent::new(
                            EventType::KEY.0,
                            PROBE_KEY.code(),
                            event.value(),
                        )]) {
                            eprintln!("forwarder write error: {e}");
                            return;
                        }
                    }
                }
            }
        })
    };

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(anyhow!("forwarding thread failed: {e}")),
        Err(_) => return Err(anyhow!("forwarding thread did not report readiness")),
    }

    let mut output = open_with_retry(&sink_node).context("opening sink for readback")?;
    output.grab().context("grabbing sink")?;

    let mut samples = Vec::with_capacity(args.count);
    let total = args.count + args.warmup;

    for i in 0..total {
        let value = i32::from(i % 2 == 0);
        let t0 = Instant::now();

        source
            .emit(&[InputEvent::new(EventType::KEY.0, PROBE_KEY.code(), value)])
            .context("emitting on source")?;

        // Block until the forwarded event surfaces on the sink.
        'wait: loop {
            for event in output.fetch_events().context("reading sink")? {
                if event.event_type() == EventType::KEY {
                    break 'wait;
                }
            }
        }

        let elapsed = t0.elapsed();
        if i >= args.warmup {
            samples.push(elapsed.as_secs_f64() * 1000.0);
        }

        spin_for(Duration::from_micros(args.interval_us));
    }

    drop(forwarder);
    report(&mut samples);
    Ok(())
}

/// A freshly created uinput device's node exists before udev has applied
/// ownership, so the first open can fail with EACCES. Retry briefly.
fn open_with_retry(path: &PathBuf) -> Result<Device> {
    let mut last = None;
    for _ in 0..50 {
        match Device::open(path) {
            Ok(d) => return Ok(d),
            Err(e) => {
                last = Some(e);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    Err(anyhow!(
        "open {} after retries: {}",
        path.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn build_device(name: &str) -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(PROBE_KEY);

    Ok(VirtualDevice::builder()?
        .name(name)
        .with_keys(&keys)?
        .build()?)
}

fn first_node(device: &mut VirtualDevice) -> Result<PathBuf> {
    device
        .enumerate_dev_nodes_blocking()?
        .next()
        .ok_or_else(|| anyhow!("virtual device exposed no event node"))?
        .map_err(Into::into)
}

/// Busy-wait rather than sleep: at 1kHz the scheduler's granularity would
/// dominate the pacing and add its own jitter to the measurement.
fn spin_for(d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn report(samples: &mut Vec<f64>) {
    if samples.is_empty() {
        println!("no samples collected");
        return;
    }

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let pct = |p: f64| samples[((n as f64 - 1.0) * p).round() as usize];

    println!("round-trip latency over {n} samples (ms)");
    println!("  min     {:.3}", samples[0]);
    println!("  p50     {:.3}", pct(0.50));
    println!("  mean    {mean:.3}");
    println!("  p95     {:.3}", pct(0.95));
    println!("  p99     {:.3}", pct(0.99));
    println!("  p99.9   {:.3}", pct(0.999));
    println!("  max     {:.3}", samples[n - 1]);
    println!();
    println!("Budget from docs/modules.md: chain <10us, daemon overhead <20us,");
    println!("kernel transits ~50-200us each. p99 above ~0.5ms warrants a look at");
    println!("HID-BPF for the sensitivity stage (the D5 escape hatch).");
}
