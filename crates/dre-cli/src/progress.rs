//! Row progress for per-row outputs and `dre release`: the measured speed, the time left, and
//! when a terminal-less run prints a line. Pure: every call takes `now`, so tests use a fake clock.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use dre_core::run::RowProgress;

/// Speed is measured over this much recent time.
const WINDOW: Duration = Duration::from_secs(30);
/// Without a terminal, a line at most this often; a job shorter than this prints none.
pub const EVERY: Duration = Duration::from_secs(30);
/// No speed (or time left) until this many rows are done.
const MIN_ROWS: u64 = 5;

/// One output's progress over time.
pub struct RowMeter {
    /// `(when, rows done)`, oldest first: the window, plus the last sample before it.
    samples: VecDeque<(Instant, u64)>,
    last_line: Instant,
}

impl RowMeter {
    pub fn new(now: Instant) -> RowMeter {
        RowMeter {
            samples: VecDeque::from([(now, 0)]),
            last_line: now,
        }
    }

    pub fn record(&mut self, now: Instant, done: u64) {
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) > WINDOW {
            self.samples.pop_front();
        }
    }

    /// Rows a second over the recent window; `None` until enough rows and time to tell.
    pub fn speed(&self, now: Instant) -> Option<f64> {
        let &(t0, d0) = self.samples.front()?;
        let &(_, d1) = self.samples.back()?;
        let secs = now.duration_since(t0).as_secs_f64();
        if d1 < MIN_ROWS || secs < 0.5 {
            return None;
        }
        Some((d1 - d0) as f64 / secs)
    }

    /// Whether a terminal-less run prints a line now: every [`EVERY`], never in the first one.
    pub fn line_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_line) >= EVERY {
            self.last_line = now;
            true
        } else {
            false
        }
    }
}

/// `1,200/20,000 rows · 3 failed · 98/min (rate 100/min) · ~3h8m left`; `sep` joins the parts.
pub fn describe(p: &RowProgress, speed: Option<f64>, sep: &str) -> String {
    let mut parts = vec![format!("{}/{} rows", thousands(p.done), thousands(p.total))];
    if p.failed > 0 {
        parts.push(format!("{} failed", thousands(p.failed)));
    }
    let unit = p.rate.map(|r| r.per_seconds);
    match (speed, p.rate) {
        (Some(s), Some(r)) => parts.push(format!("{} (rate {})", per(s, unit), rate(&r))),
        (Some(s), None) => parts.push(per(s, unit)),
        (None, Some(r)) => parts.push(format!("rate {}", rate(&r))),
        (None, None) => {}
    }
    if let Some(s) = speed.filter(|s| *s > 0.0)
        && p.done < p.total
    {
        parts.push(format!(
            "~{} left",
            duration(((p.total - p.done) as f64 / s).ceil() as u64)
        ));
    }
    parts.join(sep)
}

/// The output's name from its label (``output `email` `` → `email`), for the bar.
pub fn short_name(label: &str) -> &str {
    label.split('`').nth(1).filter(|n| !n.is_empty()).unwrap_or(label)
}

/// A speed in rows a second, in the rate's unit when there is one, else the most readable.
fn per(rows_per_sec: f64, unit: Option<u64>) -> String {
    let unit = unit.unwrap_or(if rows_per_sec >= 1.0 {
        1
    } else if rows_per_sec * 60.0 >= 1.0 {
        60
    } else {
        3600
    });
    let n = rows_per_sec * unit as f64;
    let n = if n >= 10.0 || n == n.trunc() {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    };
    format!("{n}/{}", unit_name(unit))
}

fn rate(r: &dre_core::rate::Rate) -> String {
    format!("{}/{}", thousands(r.count), unit_name(r.per_seconds))
}

fn unit_name(per_seconds: u64) -> &'static str {
    match per_seconds {
        1 => "s",
        60 => "min",
        _ => "h",
    }
}

/// `45s`, `6m12s`, `3h8m`.
fn duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dre_core::rate::Rate;

    fn progress(done: u64, total: u64, failed: u64, rate: Option<&str>) -> RowProgress {
        RowProgress {
            output: "output `email`".into(),
            done,
            failed,
            total,
            rate: rate.map(|r| Rate::parse(r).unwrap()),
            finished: false,
        }
    }

    #[test]
    fn a_line_every_30_seconds_and_none_for_a_short_job() {
        let t0 = Instant::now();
        let mut m = RowMeter::new(t0);
        assert!(
            !m.line_due(t0 + Duration::from_secs(29)),
            "a job under 30s prints no line"
        );
        assert!(m.line_due(t0 + Duration::from_secs(30)));
        assert!(!m.line_due(t0 + Duration::from_secs(45)));
        assert!(m.line_due(t0 + Duration::from_secs(61)));
    }

    #[test]
    fn speed_is_measured_over_the_recent_window_not_the_rate() {
        let t0 = Instant::now();
        let mut m = RowMeter::new(t0);
        assert_eq!(m.speed(t0), None);
        m.record(t0 + Duration::from_secs(1), 3);
        assert_eq!(
            m.speed(t0 + Duration::from_secs(1)),
            None,
            "no speed before 5 rows"
        );
        // 36 rows a second for 10 s, though the rate allows 50.
        for s in 1..=10 {
            m.record(t0 + Duration::from_secs(s), 36 * s);
        }
        let now = t0 + Duration::from_secs(10);
        let speed = m.speed(now).unwrap();
        assert!((speed - 36.0).abs() < 0.01, "{speed}");
        assert_eq!(
            describe(&progress(360, 400, 0, Some("50/second")), Some(speed), " · "),
            "360/400 rows · 36/s (rate 50/s) · ~2s left"
        );
        // A slow minute drops out of the window: the speed follows the recent pace.
        for s in 11..=100 {
            m.record(t0 + Duration::from_secs(s), 360 + (s - 10));
        }
        let speed = m.speed(t0 + Duration::from_secs(100)).unwrap();
        assert!((speed - 1.0).abs() < 0.1, "{speed}");
    }

    #[test]
    fn descriptions_read_well() {
        let p = progress(1200, 20000, 3, Some("100/minute"));
        assert_eq!(
            describe(&p, Some(98.0 / 60.0), ", "),
            "1,200/20,000 rows, 3 failed, 98/min (rate 100/min), ~3h11m left"
        );
        assert_eq!(describe(&progress(0, 400, 0, None), None, " · "), "0/400 rows");
        assert_eq!(
            describe(&progress(2, 400, 0, Some("5/second")), None, " · "),
            "2/400 rows · rate 5/s"
        );
        assert_eq!(
            describe(&progress(400, 400, 0, None), Some(12.4), " · "),
            "400/400 rows · 12/s"
        );
        assert_eq!(
            describe(&progress(10, 400, 0, None), Some(2.5), " · "),
            "10/400 rows · 2.5/s · ~2m36s left"
        );
        assert_eq!(
            describe(&progress(10, 400, 0, None), Some(0.5), " · "),
            "10/400 rows · 30/min · ~13m00s left"
        );
        assert_eq!(short_name("output `email`"), "email");
        assert_eq!(short_name("output"), "output");
    }
}
