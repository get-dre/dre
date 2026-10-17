//! Traffic control for deliveries: a destination's `rate:` (`100/minute`, `5/second`,
//! `20000/hour`), with an optional `burst:`. Deliveries are spaced evenly (`60/minute` sends one
//! a second); `burst: N` lets up to N go back to back while keeping the same average. One pacer
//! serves every worker of a run or release, so the rate holds per destination, whatever the
//! concurrency.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::CancelToken;

/// A delivery rate: `count` deliveries per `per`, and how many may go back to back.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub count: u64,
    /// The window, in seconds.
    pub per_seconds: u64,
    #[serde(default = "one")]
    pub burst: u64,
}

fn one() -> u64 {
    1
}

impl Rate {
    /// Parse `N/unit`: the unit is `second`, `minute` or `hour` (or `s`, `m`, `h`, plurals).
    pub fn parse(s: &str) -> Result<Rate, String> {
        let bad = || format!("`{s}` isn't a rate; write a count and a unit, like `100/minute`, `5/second` or `20000/hour`");
        let (n, unit) = s.split_once('/').ok_or_else(bad)?;
        let count: u64 = n.trim().replace('_', "").parse().map_err(|_| bad())?;
        if count == 0 {
            return Err(format!("`{s}`: the count must be 1 or more"));
        }
        let per_seconds = match unit.trim().to_ascii_lowercase().trim_end_matches('s') {
            "second" | "sec" | "" => 1,
            "minute" | "min" | "m" => 60,
            "hour" | "hr" | "h" => 3600,
            _ => return Err(bad()),
        };
        Ok(Rate {
            count,
            per_seconds,
            burst: 1,
        })
    }

    /// The even gap between two deliveries.
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(self.per_seconds as f64 / self.count as f64)
    }

    /// As written: `100/minute`.
    pub fn shown(&self) -> String {
        let unit = match self.per_seconds {
            1 => "second",
            60 => "minute",
            _ => "hour",
        };
        format!("{}/{unit}", self.count)
    }
}

/// Paces deliveries per destination (by a key the caller chooses).
#[derive(Default)]
pub struct Pacer {
    /// Each destination's theoretical arrival time of the next delivery.
    next: Mutex<HashMap<String, Instant>>,
}

impl Pacer {
    /// Wait for destination `key`'s next slot under `rate`. `false`: the run was cancelled meanwhile.
    pub fn wait(&self, key: &str, rate: &Rate, cancel: &CancelToken) -> bool {
        let gap = rate.interval();
        // Up to `burst` deliveries may run ahead of the even schedule.
        let slack = gap * (rate.burst.max(1) - 1) as u32;
        let until = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let tat = next.get(key).copied().unwrap_or(now).max(now);
            let allowed = tat.checked_sub(slack).map_or(now, |t| t.max(now));
            next.insert(key.to_string(), tat + gap);
            allowed
        };
        while Instant::now() < until {
            if cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep((until - Instant::now()).min(Duration::from_millis(100)));
        }
        !cancel.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rates() {
        assert_eq!(Rate::parse("100/minute").unwrap().interval(), Duration::from_millis(600));
        assert_eq!(Rate::parse("5/second").unwrap().interval(), Duration::from_millis(200));
        assert_eq!(Rate::parse("3600/hour").unwrap().interval(), Duration::from_secs(1));
        assert_eq!(Rate::parse("60 / Minutes").unwrap().shown(), "60/minute");
        assert_eq!(Rate::parse("20_000/h").unwrap().count, 20_000);
        for bad in ["100", "0/minute", "x/minute", "10/day"] {
            assert!(Rate::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spaces_evenly_and_allows_a_burst() {
        let cancel = CancelToken::new();
        let p = Pacer::default();
        let even = Rate::parse("20/second").unwrap();
        let t = Instant::now();
        for _ in 0..5 {
            assert!(p.wait("a", &even, &cancel));
        }
        // Four gaps of 50 ms.
        assert!(t.elapsed() >= Duration::from_millis(190), "{:?}", t.elapsed());
        let burst = Rate { burst: 5, ..Rate::parse("2/second").unwrap() };
        let t = Instant::now();
        for _ in 0..5 {
            assert!(p.wait("b", &burst, &cancel));
        }
        // Five back to back, then the sixth waits for the average.
        assert!(t.elapsed() < Duration::from_millis(100), "{:?}", t.elapsed());
        assert!(p.wait("b", &burst, &cancel));
        assert!(t.elapsed() >= Duration::from_millis(400), "{:?}", t.elapsed());
    }
}
