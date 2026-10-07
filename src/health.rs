//! Heartbeat and the quiet-failure check.
//!
//! Variant A of the DOP brief: `pl` itself never opens a network connection (NFR-SEC-1 forbids it
//! and an autotest checks it). So the program's job is to answer one question in its exit code and
//! let the *shell* decide who to tell:
//!
//! ```text
//! projectlife heartbeat-check || curl -fsS -o /dev/null https://hc-ping.com/<uuid>
//! ```
//!
//! The network call is made by `curl` from cron, not by `pl`. A flight recorder that stops
//! recording without saying so is worse than no recorder, which is what this exists to catch.

use crate::archive::Archive;
use crate::doctor::{Check, ERROR, OK, WARN};
use crate::events;
use crate::util;

pub const DEFAULT_MAX_AGE_SECONDS: i64 = 60;

pub struct Heartbeat {
    pub ts: Option<i64>,
    pub age_ms: Option<i64>,
    pub max_age_ms: i64,
    pub fresh: bool,
    pub daemon: Option<String>,
    pub interval_ms: i64,
    pub projects: usize,
    /// Room to write in, from the one implementation of the rule (`space`). It travels with the
    /// heartbeat because the two questions "is anything observing?" and "is anything *being
    /// recorded*?" have different answers on a full disk, and the window must be able to tell them
    /// apart: on 2026-10-06 it showed "Protected" while the daemon refused every write.
    pub storage: crate::space::Verdict,
}

impl Heartbeat {
    pub fn mode(&self) -> &'static str {
        match (self.fresh, &self.daemon) {
            (true, Some(_)) => "daemon",
            (true, None) => "external timer",
            (false, Some(_)) => "daemon (stalled)",
            (false, None) => "stopped",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fresh": self.fresh,
            "lastBeatAt": self.ts.map(util::fmt_local),
            "ageMs": self.age_ms,
            "maxAgeMs": self.max_age_ms,
            "mode": self.mode(),
            "daemonHolder": self.daemon,
            "intervalMs": self.interval_ms,
            "projects": self.projects,
            "storage": self.storage.to_json(),
        })
    }
}

pub fn heartbeat(archive: &Archive, max_age_ms: i64) -> Heartbeat {
    let ts = archive.heartbeat_ms();
    let age_ms = ts.map(|t| util::now_ms() - t);
    let fresh = age_ms.map(|a| a <= max_age_ms).unwrap_or(false);
    Heartbeat {
        ts,
        age_ms,
        max_age_ms,
        fresh,
        daemon: archive.daemon_holder().map(|i| i.raw),
        interval_ms: archive.config.i64_of("intervalSeconds", 5) * 1000,
        projects: archive.load_projects().map(|p| p.len()).unwrap_or(0),
        storage: crate::space::check(archive),
    }
}

/// Exit code for `heartbeat-check`: 0 fresh, 1 stale, 2 stale *and* a daemon claims to be running
/// (the two disagree, which is a different failure from "nothing is scheduled").
pub fn heartbeat_exit(archive: &Archive, max_age_ms: i64) -> (i32, Heartbeat) {
    let hb = heartbeat(archive, max_age_ms);
    let code = if hb.fresh {
        0
    } else if hb.daemon.is_some() {
        2
    } else {
        1
    };
    (code, hb)
}

/// What an old heartbeat means, and what the fix is, depends on *why* nothing was recorded.
///
/// On 2026-10-06 the owner's Mac had a daemon that was alive, cycling, and refusing every write,
/// because the free-space rule had been read as "whichever trips first". A refused cycle writes no
/// heartbeat (the pass is abandoned before it can run), so the archive looked like a daemon that had
/// died — and both this line and `heartbeat-check` told him to start the daemon that was already
/// running, or to run `pl scan-once --all`, which on a full disk cannot do anything at all. So: when
/// writing is stopped for want of room, that *is* the diagnosis, and the remedy names space.
fn heartbeat_finding(archive: &Archive, hb: &Heartbeat, max_age_ms: i64) -> Check {
    if hb.storage.stop {
        let what = match hb.age_ms {
            Some(age) if hb.fresh => format!(
                "writes are stopped: heartbeat {} s old (limit {} s), mode: {}",
                age / 1000,
                max_age_ms / 1000,
                hb.mode()
            ),
            Some(age) => format!(
                "nothing has been recorded: heartbeat is {} s old (limit {} s), mode: {}, and the \
                 reason is not the scheduler",
                age / 1000,
                max_age_ms / 1000,
                hb.mode()
            ),
            None => "nothing has ever been recorded on this archive".to_string(),
        };
        return Check {
            level: ERROR,
            text: format!("{what} — {}", hb.storage.reason),
            fix: Some(format!(
                "free space on the volume holding {} (pl doctor prints the numbers and the rule); \
                 writing resumes by itself, and nothing is lost: a pass that cannot store does not \
                 advance the state, so the next one records the change",
                archive.root.display()
            )),
        };
    }
    match hb.age_ms {
        Some(age) if hb.fresh => Check {
            level: OK,
            text: format!(
                "heartbeat {} s old (limit {} s), mode: {}",
                age / 1000,
                max_age_ms / 1000,
                hb.mode()
            ),
            fix: None,
        },
        Some(age) => Check {
            level: ERROR,
            text: format!(
                "heartbeat is {} s old (limit {} s), mode: {} — nothing has been observed recently",
                age / 1000,
                max_age_ms / 1000,
                hb.mode()
            ),
            fix: Some("pl scan-once --all   # or: pl daemon status / pl daemon start".into()),
        },
        None => Check {
            level: ERROR,
            text: "no heartbeat at all: neither the daemon nor scan-once has ever run against this archive".into(),
            fix: Some("pl scan-once --all   # or: pl daemon install --timer".into()),
        },
    }
}

/// `healthcheck`: doctor's findings reduced to one exit code and one line, plus the two things a
/// scheduler cares about and doctor does not say directly — is the promise being kept *now*, and
/// is any project waiting for a recovery.
pub fn health_checks(archive: &Archive, max_age_ms: i64) -> Vec<Check> {
    let hb = heartbeat(archive, max_age_ms);
    let mut out: Vec<Check> = vec![heartbeat_finding(archive, &hb, max_age_ms)];
    if hb.fresh && hb.daemon.is_none() && hb.projects > 0 {
        out.push(Check {
            level: OK,
            text: "no daemon is running, yet the heartbeat is fresh: an external timer is keeping the promise".into(),
            fix: None,
        });
    }
    for p in archive.load_projects().unwrap_or_default() {
        if p.prune_journal().is_file() {
            out.push(Check {
                level: WARN,
                text: format!("{}: an interrupted prune is waiting", p.name),
                fix: Some(format!("pl recover {}", p.name)),
            });
        }
        let unreachable = match events::load_journal(&p.dir) {
            Ok(j) => {
                let last = events::last_observed_at(&j.events);
                match last {
                    Some(t) if util::now_ms() - t > 24 * 3_600_000 => {
                        out.push(Check {
                            level: WARN,
                            text: format!("{}: not observed for {} h", p.name, (util::now_ms() - t) / 3_600_000),
                            fix: Some(format!("pl scan-once {}", p.name)),
                        });
                        false
                    }
                    _ => false,
                }
            }
            Err(_) => true,
        };
        if unreachable {
            out.push(Check {
                level: ERROR,
                text: format!("{}: the journal cannot be read", p.name),
                fix: Some(format!("pl check {}", p.name)),
            });
        }
    }
    // doctor's own findings, which cover free space, locks, missing blobs and project states.
    out.extend(crate::doctor::doctor(archive));
    out
}

/// 0 when every check is OK or a warning that does not mean "the promise is broken", 1 when
/// something is an error. Warnings keep the exit code at 0 so that a cron mail is not sent for a
/// note; `--strict` is the caller's choice, made in the shell.
pub fn worst_exit(checks: &[Check], strict: bool) -> i32 {
    if checks.iter().any(|c| c.level == ERROR) {
        1
    } else if strict && checks.iter().any(|c| c.level == WARN) {
        1
    } else {
        0
    }
}
