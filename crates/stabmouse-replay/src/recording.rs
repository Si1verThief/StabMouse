//! The recording format.
//!
//! Deliberately plain text, tab separated. A research tool's data wants to be
//! greppable, diffable and readable by a five-line Python loop — none of which a
//! binary format gives you, and none of the reasons to prefer binary (size, speed)
//! matter at a few thousand samples per stroke.
//!
//! ```text
//! # stabmouse-recording v1
//! # device	Mad Catz Global Mad Catz R.A.T. 8+ADV
//! # dpi	1600
//! # t_us	dx	dy	down
//! 1000	3	-1	0
//! 2000	1	0	1
//! ```
//!
//! `t_us` is microseconds from the source event, not from a clock read at processing
//! time. Only differences within a recording are meaningful.

use anyhow::{anyhow, Context, Result};
use std::fmt::Write as _;
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub t_us: u64,
    pub dx: i32,
    pub dy: i32,
    pub down: bool,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub device: String,
    pub dpi: f64,
    pub events: Vec<Event>,
}

impl Recording {
    pub fn header(device: &str, dpi: f64) -> String {
        format!(
            "# stabmouse-recording v{FORMAT_VERSION}\n# device\t{device}\n# dpi\t{dpi}\n# t_us\tdx\tdy\tdown\n"
        )
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        let mut device = String::from("unknown");
        let mut dpi = 1000.0;
        let mut events = Vec::new();

        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }

            if let Some(rest) = line.strip_prefix('#') {
                let mut parts = rest.trim().splitn(2, '\t');
                match (parts.next(), parts.next()) {
                    (Some("device"), Some(v)) => device = v.to_string(),
                    (Some("dpi"), Some(v)) => {
                        dpi = v.trim().parse().unwrap_or(1000.0);
                    }
                    _ => {}
                }
                continue;
            }

            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                return Err(anyhow!(
                    "{}:{}: expected 4 tab-separated columns, found {}",
                    path.display(),
                    lineno + 1,
                    cols.len()
                ));
            }

            let parse_err = |what: &str| {
                anyhow!("{}:{}: bad {what}", path.display(), lineno + 1)
            };

            events.push(Event {
                t_us: cols[0].parse().map_err(|_| parse_err("t_us"))?,
                dx: cols[1].parse().map_err(|_| parse_err("dx"))?,
                dy: cols[2].parse().map_err(|_| parse_err("dy"))?,
                down: cols[3].trim() != "0",
            });
        }

        if events.is_empty() {
            return Err(anyhow!("{} contains no samples", path.display()));
        }

        Ok(Self {
            device,
            dpi,
            events,
        })
    }

    /// Total motion in the recording, in device counts. Useful as a conservation
    /// reference when comparing variants.
    pub fn total_counts(&self) -> (i64, i64) {
        self.events.iter().fold((0, 0), |(x, y), e| {
            (x + i64::from(e.dx), y + i64::from(e.dy))
        })
    }

    pub fn duration_s(&self) -> f64 {
        match (self.events.first(), self.events.last()) {
            (Some(a), Some(b)) if b.t_us >= a.t_us => (b.t_us - a.t_us) as f64 * 1e-6,
            _ => 0.0,
        }
    }

    /// Number of distinct button-down runs.
    pub fn stroke_count(&self) -> usize {
        let mut n = 0;
        let mut was = false;
        for e in &self.events {
            if e.down && !was {
                n += 1;
            }
            was = e.down;
        }
        n
    }

    pub fn summary(&self) -> String {
        let (tx, ty) = self.total_counts();
        let mut s = String::new();
        let _ = write!(
            s,
            "{} samples over {:.2}s, {} strokes, {} dpi, net {tx}/{ty} counts, device \"{}\"",
            self.events.len(),
            self.duration_s(),
            self.stroke_count(),
            self.dpi,
            self.device
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(body: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("stabmouse-test-{}.tsv", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn round_trips_header_and_samples() {
        let body = format!(
            "{}1000\t3\t-1\t0\n2000\t0\t0\t1\n3000\t-2\t5\t1\n",
            Recording::header("test device", 1600.0)
        );
        let path = write_temp(&body);
        let r = Recording::load(&path).unwrap();

        assert_eq!(r.device, "test device");
        assert_eq!(r.dpi, 1600.0);
        assert_eq!(r.events.len(), 3);
        assert_eq!(r.total_counts(), (1, 4));
        assert_eq!(r.stroke_count(), 1);
        assert!((r.duration_s() - 0.002).abs() < 1e-9);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn malformed_lines_are_reported_with_a_line_number() {
        let body = format!("{}1000\t3\n", Recording::header("d", 800.0));
        let path = write_temp(&body);
        let err = Recording::load(&path).unwrap_err().to_string();
        assert!(err.contains(":5:"), "error should name the line: {err}");
        std::fs::remove_file(path).ok();
    }
}
