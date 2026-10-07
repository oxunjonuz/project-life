//! The daemon and the external-timer mode. Both do the same thing — one observation cycle; the only
//! difference is who starts them. A cycle takes the archive lock, so two cycles can never overlap in
//! either mode.

use crate::archive::{Archive, Project};
use crate::events;
use crate::scan::{self, ScanOptions, ScanReport};
use crate::util;
use crate::watch;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
/// Round 300 (FR-CFG-3): set by SIGHUP, read by the loop, which then re-reads `<archive>/config.json`
/// at once instead of waiting for its own next check.
static RELOAD: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
extern "C" fn on_hup(_sig: libc::c_int) {
    RELOAD.store(true, Ordering::SeqCst);
}

/// Ask the loop to stop on SIGTERM/SIGINT (unix) or on a console Ctrl-C event (Windows), and to
/// re-read the configuration on SIGHUP.
#[cfg(unix)]
pub fn install_signal_handlers() {
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = on_signal as *const () as usize;
        libc::sigaction(libc::SIGTERM, &act, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &act, std::ptr::null_mut());
        let mut hup: libc::sigaction = std::mem::zeroed();
        hup.sa_sigaction = on_hup as *const () as usize;
        libc::sigaction(libc::SIGHUP, &hup, std::ptr::null_mut());
    }
}

#[cfg(windows)]
pub fn install_signal_handlers() {
    unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
        STOP.store(true, Ordering::SeqCst);
        1
    }
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    unsafe {
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(any(unix, windows)))]
pub fn install_signal_handlers() {}

pub struct CycleResult {
    pub archive_state: String,
    pub projects: Vec<(String, Result<ScanReport, String>)>,
    pub warned_space: bool,
}

/// One-off space check: (warning, stop writing).
///
/// The arithmetic itself lives in `space`, so that the daemon, `doctor`, `healthcheck` and the
/// window cannot disagree about what "low space" means. Round 296 is what disagreement cost: the
/// daemon refused to write while the window said "Protected".
pub fn space_state(archive: &Archive) -> (bool, bool) {
    let v = crate::space::check(archive);
    (v.warn, v.stop)
}

/// How often the same warning may be repeated while the archive stays full (FR-DSK-3: every ten
/// minutes, not every cycle).
pub const FULL_REMIND_MS: i64 = 10 * 60 * 1000;

/// What the daemon last told the person about this disk, so that it can tell them once.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SpaceNotice {
    /// `ok` | `warn` | `full` | `unknown` — the last state a cycle saw.
    state: String,
    /// When writing stopped, for the "resumed after N minutes" sentence.
    since_ms: i64,
    /// When the last notification about it went out.
    last_notify_ms: i64,
}

fn space_notice_path(archive: &Archive) -> std::path::PathBuf {
    archive.root.join("logs").join("space_state.json")
}

fn read_space_notice(archive: &Archive) -> SpaceNotice {
    std::fs::read_to_string(space_notice_path(archive))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_space_notice(archive: &Archive, n: &SpaceNotice) {
    let path = space_notice_path(archive);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(n) {
        let _ = std::fs::write(path, text);
    }
}

/// FR-DSK-3, the loud half: one notification when writing stops, then at most one every ten minutes.
///
/// The state file is what makes this hold across cycles *and* across restarts: without it, every
/// five-second cycle is a fresh daemon with no memory of having just said the same thing. That is
/// exactly what the owner saw on 2026-10-06 — a notification centre full of identical warnings,
/// which is how a real warning stops being read.
pub fn announce_space(archive: &Archive, space: &crate::space::Verdict) {
    let mut n = read_space_notice(archive);
    let now = util::now_ms();
    // `full` and `unknown` are different failures, so each announces itself on entry.
    let entered = n.state != space.state;
    if entered {
        n.since_ms = now;
        n.state = space.state.to_string();
    }
    let due = n.last_notify_ms == 0 || now - n.last_notify_ms >= FULL_REMIND_MS;
    if entered || due {
        archive.log(&format!(
            "ARCHIVE_FULL: writing stopped ({}) — {}",
            if entered { "first notification" } else { "reminder after 10 minutes" },
            space.reason
        ));
        notify_full(
            archive,
            "space",
            None,
            "Project Life: recording stopped — not enough space",
            &space.reason,
        );
        n.last_notify_ms = now;
    } else {
        // Still said, but only in the log: the person is not interrupted for the fortieth time.
        archive.log(&format!(
            "ARCHIVE_FULL: writing is still stopped; next reminder in {} s — {}",
            (FULL_REMIND_MS - (now - n.last_notify_ms)) / 1000,
            space.reason
        ));
    }
    write_space_notice(archive, &n);
}

/// The other half: writing works again, and that is said exactly once.
pub fn announce_resumed(archive: &Archive, space: &crate::space::Verdict) {
    let mut n = read_space_notice(archive);
    let was_blocked = n.state == "full" || n.state == "unknown";
    if !was_blocked {
        if n.state != space.state {
            n.state = space.state.to_string();
            write_space_notice(archive, &n);
        }
        return;
    }
    let minutes = if n.since_ms > 0 {
        (util::now_ms() - n.since_ms) / 60_000
    } else {
        0
    };
    archive.log(&format!("ARCHIVE_FULL: writing resumed after {minutes} min — {}", space.reason));
    notify_full(
        archive,
        "space_resumed",
        None,
        "Project Life: recording resumed",
        &format!("writing to the archive works again. {}", space.reason),
    );
    n.state = space.state.to_string();
    n.last_notify_ms = 0;
    n.since_ms = 0;
    write_space_notice(archive, &n);
}

/// The trigger state the daemon last wrote (`watch_state.json`), for `daemon status` and the tests.
pub fn trigger_state(archive: &Archive) -> Option<Value> {
    let text = std::fs::read_to_string(archive.root.join("watch_state.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// What the last partial pass actually did (round 293), published in `watch_state.json`.
///
/// It exists so that "the notification-driven pass walked less" is a measurement and not a claim:
/// the numbers a partial pass reports — how many paths it was given, how many directories it opened,
/// how many files were in scope — are readable from outside the process.
fn partial_summary(res: &CycleResult) -> Value {
    let mut projects = Vec::new();
    for (name, r) in &res.projects {
        if let Ok(rep) = r {
            if !rep.partial {
                continue;
            }
            projects.push(serde_json::json!({
                "project": name,
                "scopePaths": rep.scope_paths,
                "dirsWalked": rep.dirs_walked,
                "filesInScope": rep.files_on_disk,
                "created": rep.created,
                "changed": rep.changed,
                "deleted": rep.deleted,
                "moved": rep.moved,
                "newBlobs": rep.blobs_new,
                "ms": rep.duration_ms,
            }));
        }
    }
    serde_json::json!({ "at": crate::archive::iso_ms(util::now_ms()), "projects": projects })
}

pub fn notify(archive: &Archive, title: &str, body: &str) {
    notify_full(archive, "notice", None, title, body);
}

/// Round 300: every notification also leaves a structured line, so "what was I told, and when" has
/// an answer later. A toast that disappears is how a person stops believing the tool tells them
/// things; the design's notification centre had no honest data behind it until this existed.
pub fn notify_full(archive: &Archive, kind: &str, project: Option<&str>, title: &str, body: &str) {
    archive.log(&format!("NOTIFY: {title} — {body}"));
    append_notification(archive, kind, project, title, body);
    if !archive.config.bool_of("notifications", true) {
        return;
    }
    #[cfg(unix)]
    {
        if cfg!(target_os = "macos") {
            let script = format!("display notification {} with title {}", shq(body), shq(title));
            let _ = std::process::Command::new("osascript").arg("-e").arg(script).output();
        } else if which("notify-send") {
            let _ = std::process::Command::new("notify-send").arg(title).arg(body).output();
        }
    }
}

fn append_notification(archive: &Archive, kind: &str, project: Option<&str>, title: &str, body: &str) {
    let dir = archive.root.join("logs");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let at = util::now_ms();
    let mut v = serde_json::json!({
        "at": at,
        "atIso": crate::archive::iso_ms(at),
        "local": util::fmt_local(at),
        "kind": kind,
        "title": title,
        "body": body,
    });
    if let Some(p) = project {
        v["project"] = Value::from(p);
    }
    let mut line = serde_json::to_string(&v).unwrap_or_default();
    line.push('\n');
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("notifications.jsonl"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Read the ledger. Bounded on purpose: a file that has been appended to for years must not be pulled
/// into memory whole just to show the last twenty lines.
pub fn read_notifications(archive: &Archive, limit: usize) -> Vec<Value> {
    const TAIL_BYTES: u64 = 512 * 1024;
    let path = archive.root.join("logs").join("notifications.jsonl");
    let mut rows: Vec<Value> = Vec::new();
    let mut text = String::new();
    if let Ok(mut f) = fs::File::open(&path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len > TAIL_BYTES {
            use std::io::Seek;
            let _ = f.seek(std::io::SeekFrom::Start(len - TAIL_BYTES));
            // The first line of the window may be a fragment: dropped below by the parse check.
        }
        if f.read_to_string(&mut text).is_err() {
            return rows;
        }
    }
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            rows.push(v);
        }
    }
    if rows.len() > limit {
        rows.drain(0..rows.len() - limit);
    }
    rows
}

#[cfg(unix)]
fn shq(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(unix)]
fn which(prog: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for p in path.split(':') {
            if std::path::Path::new(p).join(prog).is_file() {
                return true;
            }
        }
    }
    false
}

/// One cycle over the archive (all projects, or just one).
///
/// `count_skipped` is only true for `scan-once --skipped`: it makes the walk size every skipped
/// directory, which is a traversal of its own and must not happen in the ordinary cycle.
pub fn run_cycle(
    archive: &Archive,
    only: Option<&str>,
    reason: &str,
    deep: bool,
    count_skipped: bool,
) -> Result<CycleResult, String> {
    run_cycle_inner(archive, only, reason, deep, count_skipped, None, None)
}

/// A cycle that was started by filesystem notifications carries the paths that triggered it, so the
/// log says *why* this pass happened and not only that one did.
pub fn run_cycle_triggered(
    archive: &Archive,
    only: Option<&str>,
    reason: &str,
    deep: bool,
    count_skipped: bool,
    trigger: Option<&[String]>,
) -> Result<CycleResult, String> {
    run_cycle_inner(archive, only, reason, deep, count_skipped, trigger, None)
}

/// A **partial** cycle (round 293): the notification paths decide what is walked, not only when.
///
/// `by_project` maps a project name to the notification paths that arrived for it. Projects with no
/// notification in this pass are not touched at all — that is the whole point of the pass — and a
/// project whose paths are all in another project's tree does no work. Everything else (the lock,
/// the heartbeat, the journal, the blobs, the cache, the mass thresholds) is the ordinary path.
pub fn run_partial_cycle(
    archive: &Archive,
    by_project: &BTreeMap<String, Vec<String>>,
    trigger: &[String],
) -> Result<CycleResult, String> {
    run_cycle_inner(archive, None, "changed", false, false, Some(trigger), Some(by_project))
}

/// Split the trigger labels (`project:relative/path`) into per-project path lists.
///
/// Returns `(by_project, must_be_full)`. `must_be_full` is true when a label cannot be read as a
/// path at all — the queue-overflow marker is the one that really happens — and then the pass must
/// be a full one: notifications were lost, so "only what was mentioned" is exactly the wrong scope.
pub fn group_trigger_paths(labels: &[String]) -> (BTreeMap<String, Vec<String>>, bool) {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut must_be_full = false;
    for l in labels {
        if l.starts_with('<') {
            must_be_full = true;
            continue;
        }
        match l.split_once(':') {
            Some((name, rel)) if !name.is_empty() => {
                out.entry(name.to_string()).or_default().push(scan::normalize_rel(rel));
            }
            _ => must_be_full = true,
        }
    }
    (out, must_be_full)
}

fn run_cycle_inner(
    archive: &Archive,
    only: Option<&str>,
    reason: &str,
    deep: bool,
    count_skipped: bool,
    trigger: Option<&[String]>,
    partial: Option<&BTreeMap<String, Vec<String>>>,
) -> Result<CycleResult, String> {
    if let Some(paths) = trigger {
        let mut list: Vec<String> = paths.iter().take(8).cloned().collect();
        if paths.len() > list.len() {
            list.push(format!("… and {} more", paths.len() - list.len()));
        }
        let kind = if partial.is_some() { "partial" } else { "full" };
        archive.log(&format!(
            "cycle triggered by filesystem notifications ({kind} pass): {} path(s): {}",
            paths.len(),
            list.join(", ")
        ));
    }
    if !archive.root.is_dir() {
        archive.log("ARCHIVE_OFFLINE: archive root is unreachable");
        notify_full(archive, "offline", None, "Project Life: archive unreachable", &format!("{} is not readable", archive.root.display()));
        return Ok(CycleResult { archive_state: "ARCHIVE_OFFLINE".into(), projects: Vec::new(), warned_space: false });
    }
    let space = crate::space::check(archive);
    let (warn, stop) = (space.warn, space.stop);
    if stop {
        // The sentence carries the two numbers and the rule that produced them: "no space" without
        // the arithmetic is a rumour, and on a 926 GB volume it was a wrong one.
        //
        // And it is said **once**, not once per cycle: FR-DSK-3 asks for a notification on the
        // transition and then at most one every ten minutes. On 2026-10-06 the Mac's notification
        // centre filled with the same sentence, because a five-second cycle told it five seconds
        // apart, and a person who sees forty identical warnings learns to ignore all of them.
        announce_space(archive, &space);
        return Ok(CycleResult { archive_state: "ARCHIVE_FULL".into(), projects: Vec::new(), warned_space: true });
    }
    announce_resumed(archive, &space);
    if warn {
        archive.log(&format!("low free space — {}", space.reason));
    }
    let lock = archive.lock("cycle")?;
    let mut targets: Vec<Project> = match only {
        Some(name) => vec![archive.find(name)?],
        None => archive.load_projects()?,
    };
    let interval = archive.config.i64_of("intervalSeconds", 5) * 1000;
    let mut results = Vec::new();
    for project in targets.iter_mut() {
        // A partial pass does no work for a project no notification mentioned: no journal read, no
        // cache read, no walk. This `continue` is the difference between "walk less" and "walk
        // nothing", and it is why a busy archive's other projects cost nothing here.
        let scoped_paths: Option<Vec<String>> = match partial {
            Some(map) => match map.get(&project.name) {
                Some(v) if !v.is_empty() => Some(v.clone()),
                _ => continue,
            },
            None => None,
        };
        if only.is_none() {
            let st = project.state();
            if st == "paused" || st == "removed" {
                continue;
            }
        } else if project.state() == "removed" {
            return Err(format!("project {} was removed from observation (remove)", project.name));
        }
        if project.state() == "error" {
            archive.log(&format!("{}: project is in state error, not observing", project.name));
            continue;
        }
        // Observation gap: written before reconciliation.
        //
        // Round 294: this used to read the whole journal on every cycle, including a partial one,
        // just to learn when the project was last observed. The tail carries that (it is refreshed by
        // every append), and `project.json` carries it too; the journal is read in full only if the
        // tail cannot be trusted.
        let tail = events::tail(project).ok();
        let last = tail
            .as_ref()
            .and_then(|t| t.observed_at)
            .or_else(|| project.meta_str("lastObservedAt").and_then(|s| crate::archive::parse_iso_ms(&s)));
        if let Some(prev) = last {
            let now = util::now_ms();
            if now - prev > 2 * interval.max(1000) {
                let mut e = events::ev_new(0, now, "gap");
                events::put_i64(&mut e, "from", prev);
                events::put_i64(&mut e, "to", now);
                events::put_str(&mut e, "reason", reason);
                let mut v = vec![e];
                let _ = events::append(project, &mut v);
                project.set_meta("lastGapAt", Value::from(crate::archive::iso_ms(now)));
                let _ = project.save_meta();
            }
        }
        let opts = ScanOptions {
            reason: reason.to_string(),
            deep,
            verbose_filters: false,
            dry_run: false,
            with_initial_snapshot: false,
            count_skipped,
            scope: scoped_paths
                .as_ref()
                .map(|paths| scan::Scope::from_paths(&project.project_path(), paths)),
        };
        let res = scan::scan_project(archive, project, &opts);
        if let Ok(rep) = &res {
            if rep.partial {
                archive.log(&format!(
                    "{}: partial pass over {} notification path(s) — {} director(ies) walked, {} file(s) in scope, +{} ~{} -{} moved {} ({} ms)",
                    project.name, rep.scope_paths, rep.dirs_walked, rep.files_on_disk,
                    rep.created, rep.changed, rep.deleted, rep.moved, rep.duration_ms
                ));
            }
        }
        if let Ok(rep) = &res {
            if let Some(mass) = &rep.mass {
                let kind = events::get_str(mass, "kind").unwrap_or_else(|| "mass".into());
                let files = events::get_u64(mass, "files").unwrap_or(0);
                let good_seq = events::get_u64(mass, "lastGoodSeq").unwrap_or(0);
                let good_ts = match events::load_journal(&project.dir) {
                    // Only a mass event needs this, and it is rare: the ts of the last event that is
                    // part of the last good state. Everything else on this path avoids the journal.
                    Ok(j) => j
                        .events
                        .iter()
                        .filter(|e| events::seq_of(e) <= good_seq)
                        .map(events::ts_of)
                        .max()
                        .unwrap_or(util::now_ms()),
                    Err(_) => util::now_ms(),
                };
                let body = format!(
                    "«{kind}» in project {}: {} files at {}.\nLast good state: {}.\nRestore with: pl restore {} --at \"{}\" --to ../{}-recovered   (or pl panic {})\nPut back only what is gone: pl restore {} --at \"{}\" --missing --into-project --yes",
                    project.name,
                    files,
                    util::fmt_local(util::now_ms()),
                    util::fmt_local(good_ts),
                    project.name,
                    util::fmt_local(good_ts),
                    project.name,
                    project.name,
                    project.name,
                    util::fmt_local(good_ts)
                );
                notify_full(archive, "mass", Some(&project.name), "Project Life: mass change", &body);
            }
        } else if let Err(e) = &res {
            archive.log(&format!("{}: {e}", project.name));
        }
        results.push((project.name.clone(), res));
    }
    archive.write_heartbeat(util::now_ms());
    lock.release();
    Ok(CycleResult { archive_state: "OK".into(), projects: results, warned_space: warn })
}

/// FR-CFG-3: apply the keys the loop holds in local variables. Everything else in the configuration
/// is read per cycle by the cycle itself and is therefore already live.
fn apply_live_config(
    archive: &Archive,
    changed: &[String],
    interval_ms: &mut i64,
    next_periodic: &mut i64,
    debounce_ms: &mut i64,
    resync_cycles: &mut i64,
    triggers: Option<bool>,
    watcher: &mut watch::Watcher,
) {
    let has = |k: &str| changed.iter().any(|c| c == k);
    if has("intervalSeconds") || has("minIntervalSeconds") || has("maxIntervalSeconds") || has("autoInterval") {
        let new_ms = archive.config.i64_of("intervalSeconds", 5).max(1) * 1000;
        if new_ms != *interval_ms {
            archive.log(&format!(
                "interval {interval_ms} ms -> {new_ms} ms (applied without a restart)"
            ));
            *interval_ms = new_ms;
        }
        // The periodic deadline is measured from the end of the last pass, but a shortened interval
        // must not wait for the old, longer one to expire: reschedule from now.
        *next_periodic = util::now_ms() + (*interval_ms).max(200);
    }
    if has("debounceMs") {
        *debounce_ms = archive.config.i64_of("debounceMs", 1500).clamp(0, 600_000);
    }
    if has("triggerResyncCycles") {
        *resync_cycles = archive.config.i64_of("triggerResyncCycles", 12).max(1);
    }
    if has("watchTriggers") && triggers.is_none() {
        let want = archive.config.bool_of("watchTriggers", true);
        if watcher.set_enabled(want) {
            if watcher.active() {
                match watcher.refresh(archive) {
                    Ok(n) => archive.log(&format!("trigger: turned on without a restart — watching {n} directories")),
                    Err(e) => archive.log(&format!("trigger: turned on but the watches could not be installed: {e}")),
                }
            } else {
                archive.log(&format!("trigger: turned off without a restart ({})", watcher.describe()));
            }
        }
    }
}

/// The resident loop.
///
/// Two triggers, one pass. The periodic full pass is the backbone and is never postponed by a
/// notification; filesystem notifications only make the same pass happen *sooner* when something
/// changed. The debounce is per path: a burst of changes to one file waits for 1.5 s of quiet, but
/// no change can wait longer than 2 × interval, so a process that writes continuously cannot
/// postpone storage for ever (FR-WCH-6).
pub fn run_daemon(archive: &mut Archive, triggers: Option<bool>) -> Result<(), String> {
    install_signal_handlers();
    // FR-WCH-12: a second daemon exits with a clear message instead of racing the first one.
    let guard = archive.daemon_lock("daemon run")?;
    archive.log("daemon started");
    // A request that outlived the daemon it was written for must not stop this one: it is read and
    // removed here, and the log says what it said.
    if let Some(stale) = archive.clear_stale_stop_request() {
        archive.log(&format!(
            "ignoring a stop request left behind (\"{stale}\"): it does not name this process"
        ));
    }
    notify_full(archive, "lifecycle", None, "Project Life", "Observation started");
    archive.write_config_state(&[], "start");

    let triggers_enabled = triggers.unwrap_or_else(|| archive.config.bool_of("watchTriggers", true));
    let mut debounce_ms = archive.config.i64_of("debounceMs", 1500).clamp(0, 600_000);
    let mut resync_cycles = archive.config.i64_of("triggerResyncCycles", 12).max(1);
    let mut watcher = watch::Watcher::install(triggers_enabled);
    if watcher.active() {
        match watcher.refresh(archive) {
            Ok(n) => archive.log(&format!("trigger: watching {n} directories for filesystem notifications")),
            Err(e) => archive.log(&format!("trigger: could not install filesystem watches: {e}")),
        }
    } else {
        archive.log(&format!(
            "trigger: {} — the periodic pass is the only trigger",
            watcher.describe()
        ));
    }
    watcher.write_state(archive);

    let mut failures = 0u32;
    let mut interval_ms = archive.config.i64_of("intervalSeconds", 5).max(1) * 1000;
    let mut next_periodic = util::now_ms();
    let mut cycles_since_resync = 0i64;
    // Per path: (first event, last event) in ms. The debounce is measured from these.
    let mut dirty: std::collections::BTreeMap<String, (i64, i64)> = std::collections::BTreeMap::new();

    // Why this daemon left, for the last line it writes. A signal is not the only way to stop and
    // the log must not claim it was.
    let mut stop_why = "signal";
    while !STOP.load(Ordering::SeqCst) {
        // ---- A stop request, which is how every platform can be asked, signals or not ----
        if archive.take_stop_request(std::process::id() as i32).is_some() {
            stop_why = "request";
            archive.log(
                "stop requested: the request file names this process, so this daemon is leaving \
                 between passes",
            );
            break;
        }
        // ---- FR-CFG-3: re-read the configuration, on SIGHUP or on a change of its bytes ----
        let signalled = RELOAD.swap(false, Ordering::SeqCst);
        match archive.reload_config() {
            Some(changed) => {
                let why = if signalled { "SIGHUP" } else { "config.json changed" };
                archive.write_config_state(&changed, why);
                apply_live_config(
                    archive,
                    &changed,
                    &mut interval_ms,
                    &mut next_periodic,
                    &mut debounce_ms,
                    &mut resync_cycles,
                    triggers,
                    &mut watcher,
                );
            }
            None if signalled => archive.log(
                "SIGHUP: the configuration file is unchanged, so nothing was re-applied",
            ),
            None => {}
        }
        // ---- how long may we sleep, and does a trigger fire before the next periodic pass? ----
        let quiet_at = dirty.values().map(|(_, l)| *l + debounce_ms).max();
        let cap_at = dirty.values().map(|(f, _)| *f + 2 * interval_ms).min();
        let trigger_at = match (quiet_at, cap_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let deadline = match trigger_at {
            Some(t) => next_periodic.min(t),
            None => next_periodic,
        };
        let wait_ms = (deadline - util::now_ms()).clamp(0, 1000);
        if wait_ms > 0 {
            watcher.wait(wait_ms);
        }
        if watcher.collect() > 0 {
            let ts = util::now_ms();
            for path in watcher.take_pending() {
                let e = dirty.entry(path).or_insert((ts, ts));
                e.1 = ts;
            }
            watcher.write_state(archive);
        }
        let now = util::now_ms();
        let trigger_due = !dirty.is_empty()
            && (dirty.values().map(|(_, l)| *l + debounce_ms).max().map(|t| now >= t).unwrap_or(false)
                || dirty.values().map(|(f, _)| *f + 2 * interval_ms).min().map(|t| now >= t).unwrap_or(false));
        let periodic_due = now >= next_periodic;
        if !trigger_due && !periodic_due {
            continue;
        }
        let changed: Vec<String> = dirty.keys().cloned().collect();
        dirty.clear();
        if trigger_due {
            watcher.trigger_cycles += 1;
        }
        if periodic_due {
            watcher.periodic_cycles += 1;
        }
        if trigger_due && periodic_due {
            archive.log("the periodic pass and a notification came due together: one full pass covers both");
        }
        let paths: Option<&[String]> = if trigger_due { Some(changed.as_slice()) } else { None };
        // Round 293: a notification-driven pass walks only the paths it was told about. The periodic
        // pass is still the full one and a notification never postpones it — if both are due, one
        // full pass covers both. Three things make a notification-driven pass a full one: the queue
        // overflowed (notifications were lost, so "only what was mentioned" is exactly the wrong
        // scope), a label cannot be read as a path, and `partialPass=false` (the documented switch
        // that restores the round-290 behaviour).
        let (by_project, unreadable_labels) =
            if trigger_due { group_trigger_paths(&changed) } else { (BTreeMap::new(), false) };
        let partial_wanted = trigger_due
            && !periodic_due
            && archive.config.bool_of("partialPass", true)
            && !unreadable_labels
            && !by_project.is_empty();
        if trigger_due && !periodic_due {
            if partial_wanted {
                watcher.partial_cycles += 1;
            } else {
                archive.log(&format!(
                    "a notification-driven pass runs as a full pass (queue overflow, unreadable label, or partialPass=false); {} label(s) arrived",
                    changed.len()
                ));
            }
        }

        let started = util::now_ms();
        let reason = if trigger_due { "changed" } else { "observed" };
        let outcome = if partial_wanted {
            run_partial_cycle(archive, &by_project, &changed)
        } else {
            run_cycle_triggered(archive, None, reason, false, false, paths)
        };
        match outcome {
            Ok(res) => {
                if partial_wanted {
                    watcher.set_last_partial(partial_summary(&res));
                }
                if res.archive_state != "OK" {
                    archive.log(&format!("archive state: {}", res.archive_state));
                }
                failures = 0;
                let deep_min = archive.config.i64_of("deepVerifyIntervalMinutes", 60);
                if deep_min > 0 {
                    let last_deep = std::fs::read_to_string(archive.root.join("last_deep_verify"))
                        .ok()
                        .and_then(|s| s.trim().parse::<i64>().ok())
                        .unwrap_or(0);
                    if util::now_ms() - last_deep > deep_min * 60_000 {
                        let _ = run_cycle(archive, None, "deep_verify", true, false);
                        let _ = util::write_atomic(
                            &archive.root.join("last_deep_verify"),
                            util::now_ms().to_string().as_bytes(),
                        );
                    }
                }
            }
            Err(e) => {
                failures += 1;
                archive.log(&format!("cycle failed ({failures}): {e}"));
                if failures == 3 {
                    notify_full(archive, "error", None, "Project Life: cycle error", &e);
                }
            }
        }
        let elapsed = util::now_ms() - started;
        if archive.config.bool_of("autoInterval", true) {
            let maxi = archive.config.i64_of("maxIntervalSeconds", 60) * 1000;
            if elapsed > interval_ms {
                interval_ms = (elapsed * 3).min(maxi).max(interval_ms);
                archive.log(&format!("a full pass took {elapsed} ms — interval raised to {interval_ms} ms"));
            }
        }
        // The periodic deadline is measured from the END of the last pass of any kind, so the
        // interval is a bound on the gap between two passes and a trigger can only shorten it.
        next_periodic = util::now_ms() + interval_ms.max(200);
        cycles_since_resync += 1;
        if cycles_since_resync >= resync_cycles {
            cycles_since_resync = 0;
            if watcher.active() {
                if let Err(e) = watcher.refresh(archive) {
                    archive.log(&format!("trigger: could not re-sync the watch set: {e}"));
                }
            }
        }
        watcher.write_state(archive);
    }
    archive.log(&format!("daemon stopped on {stop_why}"));
    let _ = std::fs::remove_file(archive.root.join(".lock"));
    watcher.write_state(archive);
    guard.release();
    Ok(())
}
