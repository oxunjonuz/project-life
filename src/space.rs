//! How much room must be left before the promise is kept, and what to say when there is not.
//!
//! FR-DSK-3 says it in one sentence, and the sentence has a trap in it:
//!
//! > Free space: below the warning threshold (**10 % or 5 GB, whichever is smaller**) a notification
//! > and a mark in `status`; below the stop threshold (**1 % or 500 MB, whichever is smaller**) new
//! > versions stop being written.
//!
//! "Whichever is smaller" is the rule, and round 296 found what happens when it is read as "whichever
//! trips first": on the owner's Mac — a 926 GB volume with 1.5–2.5 GB free — `free < 500 MB || free% < 1`
//! asked whether 1.8 GB is less than 1 % of 926 GB (9.3 GB). It is, so the archive was declared FULL,
//! the daemon stopped recording, and the whole point of the product stopped with it. The threshold is
//! the *smaller* of the two numbers, so on a big volume the fixed number governs and the percentage
//! only takes over when the volume is small enough for it to matter.
//!
//! This module exists once, for the daemon, the doctor, `healthcheck` and the window. Round 296 is a
//! case of two copies of one formula drifting apart: the window said "Protected" while the daemon was
//! refusing to write, because the window asked a different question.

use crate::archive::{Archive, Config};
use crate::util;
use serde_json::{json, Value};

pub const DEFAULT_WARN_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const DEFAULT_WARN_PERCENT: u64 = 10;
pub const DEFAULT_STOP_BYTES: u64 = 500 * 1024 * 1024;
pub const DEFAULT_STOP_PERCENT: u64 = 1;

pub struct Limits {
    pub warn_bytes: u64,
    pub warn_pct: u64,
    pub stop_bytes: u64,
    pub stop_pct: u64,
}

pub fn limits(config: &Config) -> Limits {
    Limits {
        warn_bytes: config.u64_of("warnFreeBytes", DEFAULT_WARN_BYTES),
        warn_pct: config.u64_of("warnFreePercent", DEFAULT_WARN_PERCENT),
        stop_bytes: config.u64_of("stopFreeBytes", DEFAULT_STOP_BYTES),
        stop_pct: config.u64_of("stopFreePercent", DEFAULT_STOP_PERCENT),
    }
}

/// The smaller of a fixed number of bytes and a percentage of the volume.
///
/// An unknown volume size is not a zero: when `total` is unknown the percentage cannot be computed,
/// so the fixed number stands alone. Treating "unknown" as 0 would make the percentage governing and
/// stop writing on a volume of unknown size — the same class of error this module was written to end.
pub fn threshold(bytes: u64, pct: u64, total: Option<u64>) -> u64 {
    match total {
        Some(t) if t > 0 => {
            let by_pct = ((t as u128) * (pct as u128) / 100) as u64;
            bytes.min(by_pct)
        }
        _ => bytes,
    }
}

pub struct Verdict {
    pub free: Option<u64>,
    pub total: Option<u64>,
    pub warn_threshold: Option<u64>,
    pub stop_threshold: Option<u64>,
    pub warn: bool,
    pub stop: bool,
    /// `ok` | `warn` | `full` | `unknown`
    pub state: &'static str,
    /// One sentence, with the two numbers and the rule that produced them. Never generic.
    pub reason: String,
}

impl Verdict {
    pub fn to_json(&self) -> Value {
        let pct = match (self.free, self.total) {
            (Some(f), Some(t)) if t > 0 => json!((f as f64) * 100.0 / (t as f64)),
            _ => Value::Null,
        };
        json!({
            "state": self.state,
            "freeBytes": self.free,
            "totalBytes": self.total,
            "freePercent": pct,
            "warnBytes": self.warn_threshold,
            "stopBytes": self.stop_threshold,
            "warn": self.warn,
            "stop": self.stop,
            "reason": self.reason,
        })
    }
}

fn pct_of(free: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (free as f64) * 100.0 / (total as f64)
    }
}

/// The rule, as pure arithmetic: no filesystem, no clock, no config file — so it can be tested with
/// the numbers from a real machine instead of the numbers of whatever disk this runs on.
pub fn verdict(free: Option<u64>, total: Option<u64>, lim: &Limits) -> Verdict {
    let free = match free {
        Some(f) => f,
        None => {
            return Verdict {
                free: None,
                total,
                warn_threshold: None,
                stop_threshold: None,
                warn: false,
                stop: true,
                state: "unknown",
                reason: "free space could not be determined for this volume (the archive is not \
                         reachable?): writing is stopped rather than guessed at"
                    .into(),
            }
        }
    };
    let warn_threshold = threshold(lim.warn_bytes, lim.warn_pct, total);
    let stop_threshold = threshold(lim.stop_bytes, lim.stop_pct, total);
    let stop = free < stop_threshold;
    let warn = !stop && free < warn_threshold;
    let shown = match total {
        Some(t) => format!(
            "{} ({} of the volume)",
            util::human_size(free),
            format!("{:.1} %", pct_of(free, t))
        ),
        None => util::human_size(free),
    };
    let rule = |bytes: u64, pct: u64, threshold: u64, total: Option<u64>| -> String {
        match total {
            Some(t) if t > 0 => {
                let by_pct = ((t as u128) * (pct as u128) / 100) as u64;
                if by_pct < bytes {
                    format!(
                        "{} ({} % of {} — the smaller of {} and {} %)",
                        util::human_size(threshold),
                        pct,
                        util::human_size(t),
                        util::human_size(bytes),
                        pct
                    )
                } else {
                    format!(
                        "{} (the smaller of {} and {} % = {})",
                        util::human_size(threshold),
                        util::human_size(bytes),
                        pct,
                        util::human_size(by_pct)
                    )
                }
            }
            _ => format!(
                "{} (the configured floor; the volume size is unknown)",
                util::human_size(threshold)
            ),
        }
    };
    if stop {
        return Verdict {
            free: Some(free),
            total,
            warn_threshold: Some(warn_threshold),
            stop_threshold: Some(stop_threshold),
            warn: false,
            stop: true,
            state: "full",
            reason: format!(
                "free space {shown} is below the stop threshold {}: new versions are not written; \
                 checking continues, writing resumes when space is freed, and nothing is deleted",
                rule(lim.stop_bytes, lim.stop_pct, stop_threshold, total)
            ),
        };
    }
    if warn {
        return Verdict {
            free: Some(free),
            total,
            warn_threshold: Some(warn_threshold),
            stop_threshold: Some(stop_threshold),
            warn: true,
            stop: false,
            state: "warn",
            reason: format!(
                "free space {shown} is below the warning threshold {}: versions are still being \
                 written (they stop below {})",
                rule(lim.warn_bytes, lim.warn_pct, warn_threshold, total),
                rule(lim.stop_bytes, lim.stop_pct, stop_threshold, total)
            ),
        };
    }
    Verdict {
        free: Some(free),
        total,
        warn_threshold: Some(warn_threshold),
        stop_threshold: Some(stop_threshold),
        warn: false,
        stop: false,
        state: "ok",
        reason: format!(
            "free space {shown} is above the stop threshold {}",
            rule(lim.stop_bytes, lim.stop_pct, stop_threshold, total)
        ),
    }
}

/// The verdict for a real archive: the same numbers the daemon obeys and the window shows.
pub fn check(archive: &Archive) -> Verdict {
    verdict(
        archive.free_bytes(),
        archive.total_bytes(),
        &limits(&archive.config),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Limits {
        Limits {
            warn_bytes: DEFAULT_WARN_BYTES,
            warn_pct: DEFAULT_WARN_PERCENT,
            stop_bytes: DEFAULT_STOP_BYTES,
            stop_pct: DEFAULT_STOP_PERCENT,
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    /// The owner's Mac, measured on 2026-10-06: a 926 GB volume with 1.5–2.5 GB free. The old rule
    /// (`free < 500 MB || free % < 1`) stopped writing here, because 1.8 GB is less than 1 % of
    /// 926 GB. It must not: 1.8 GB is above the smaller of the two thresholds, which is 500 MB.
    #[test]
    fn on_a_926_gb_volume_1_8_gb_free_is_not_a_stop() {
        let mut lim = defaults();
        lim.warn_bytes = 5 * GIB;
        let v = verdict(Some(1800 * MIB), Some(926 * GIB), &lim);
        assert!(!v.stop, "the archive must not be declared full: {}", v.reason);
        assert_eq!(
            v.state, "warn",
            "it is below the 5 GB warning threshold, and that is all: {}",
            v.reason
        );
        assert_eq!(v.stop_threshold, Some(DEFAULT_STOP_BYTES));
        assert!(
            v.reason.contains("5.0 GB") && v.reason.contains("10 %"),
            "the warning reason names the threshold and the rule: {}",
            v.reason
        );
        // And the stop branch says the same thing about its own two numbers.
        let stopped = verdict(Some(300 * MIB), Some(926 * GIB), &lim);
        assert!(stopped.stop);
        assert!(stopped.reason.contains("500.0 MB"), "{}", stopped.reason);
        assert!(stopped.reason.contains("9.3 GB"), "the percentage it lost to: {}", stopped.reason);
    }

    /// And the percentage must still govern when the volume is small — otherwise the rule would
    /// simply be "ignore the percentage", which is not what the specification says.
    #[test]
    fn on_a_small_volume_the_percentage_is_the_smaller_number_and_governs() {
        let lim = defaults();
        let total = 8 * GIB;
        let expected = total / 100; // 1 %
        assert_eq!(threshold(DEFAULT_STOP_BYTES, 1, Some(total)), expected);
        assert!(expected < DEFAULT_STOP_BYTES);
        let v = verdict(Some(expected - MIB), Some(total), &lim);
        assert!(v.stop, "below 1 % of a small volume: {}", v.reason);
        assert!(v.reason.contains("1 % of 8.0 GB"), "the reason says which rule won: {}", v.reason);
    }

    /// The discriminating case, on a volume where the two rules disagree: 800 MB is below 1 % of a
    /// 100 GB volume (1.0 GB) but above the fixed floor (500 MB). Reading the specification as
    /// "whichever trips first" stops here; "whichever is smaller" does not.
    #[test]
    fn the_smaller_number_wins_where_the_two_rules_disagree() {
        let lim = defaults();
        let total = 100 * GIB;
        assert!(threshold(DEFAULT_STOP_BYTES, 1, Some(total)) == DEFAULT_STOP_BYTES);
        let v = verdict(Some(800 * MIB), Some(total), &lim);
        assert!(!v.stop, "500 MB is the smaller number, so it is the threshold: {}", v.reason);
        assert_eq!(v.state, "warn", "below 5 GB warns, and warning is not stopping: {}", v.reason);
        assert!(
            v.reason.contains("the smaller of 500.0 MB and 1 %"),
            "the reason states which rule won: {}",
            v.reason
        );
    }

    #[test]
    fn an_unknown_volume_size_leaves_the_fixed_floor_alone() {
        // A percentage of an unknown size must not become zero, which would stop every write.
        assert_eq!(threshold(DEFAULT_STOP_BYTES, 1, None), DEFAULT_STOP_BYTES);
        assert_eq!(threshold(DEFAULT_STOP_BYTES, 1, Some(0)), DEFAULT_STOP_BYTES);
    }

    #[test]
    fn free_space_that_cannot_be_read_stops_writing_and_says_so() {
        let v = verdict(None, Some(GIB), &defaults());
        assert!(v.stop);
        assert_eq!(v.state, "unknown");
        assert!(v.reason.contains("could not be determined"), "{}", v.reason);
    }

    #[test]
    fn a_healthy_volume_reports_the_rule_it_used() {
        let v = verdict(Some(850 * GIB), Some(926 * GIB), &defaults());
        assert_eq!(v.state, "ok");
        assert!(!v.stop && !v.warn);
        assert!(v.reason.contains("500"), "the reason states the stop threshold: {}", v.reason);
        let j = v.to_json();
        assert_eq!(j["state"], "ok");
        assert_eq!(j["stop"], false);
        assert!(j["freePercent"].as_f64().unwrap() > 90.0);
    }
}
