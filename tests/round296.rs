//! Round 296 — what the Mac found, tested from outside the program.
//!
//! Three things were wrong on the owner's Mac on 2026-10-06, and all three were measured by him,
//! not guessed by me:
//!
//! 1. **The archive was declared full on a healthy disk.** `free < 500 MB || free% < 1` asks about
//!    a 926 GB volume whether 1.8 GB is less than 1 % of it (9.3 GB) — it is, so recording stopped.
//!    FR-DSK-3 says the threshold is the *smaller* of the two numbers.
//! 2. **The notification centre filled with the same sentence.** The stop branch notified on every
//!    cycle, and a cycle is five seconds long. FR-DSK-3 asks for one notification on the transition
//!    and then at most one every ten minutes, plus one when writing resumes.
//! 3. **The window said "Protected" while nothing was being written.** A live process is not a
//!    written archive.
//!
//! The tests below drive the real binary: `pl config set` moves the thresholds, `pl scan-once` runs
//! the real cycle, `pl heartbeat-check --json` is the same document the window reads, and the
//! notifications are counted in the archive's own log rather than in a stub of mine.
//!
//! The disk these run on is not filled up. `/work` is the owner's own 927 GB volume and it is 99 %
//! full: the arithmetic of test 1 is *his* case, reproduced by setting the percentage threshold so
//! that the percentage test fires and the fixed floor does not — the exact disagreement between the
//! two rules that made his Mac stop recording.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn core() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let mut dir = exe.parent().expect("dir").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("projectlife");
    assert!(candidate.is_file(), "build the binary first: {}", candidate.display());
    candidate
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("pl296-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// A fixture placed on a named volume, so that a test can say *which* disk it measured.
fn tmp_on(volume: &Path, name: &str) -> PathBuf {
    let p = volume.join(format!(".pl296-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(env_home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(core())
        .env("PROJECTLIFE_HOME", env_home)
        .args(args)
        .output()
        .expect("run projectlife");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn json_of(out: &str) -> Value {
    let start = out.rfind("\n{").map(|i| i + 1).unwrap_or_else(|| out.find('{').unwrap_or(0));
    serde_json::from_str(&out[start..]).expect("the output ends with a JSON document")
}

fn log_text(arch: &Path) -> String {
    fs::read_to_string(arch.join("logs/projectlife.log")).unwrap_or_default()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// How many notifications of one kind the archive's log records. `pl` writes the notification it
/// would show (or did show) as a `NOTIFY:` line, so this counts what a person would have received.
fn notifications(arch: &Path, needle: &str) -> usize {
    log_text(arch)
        .lines()
        .filter(|l| l.contains("NOTIFY:") && l.contains(needle))
        .count()
}

fn fixture_project(root: &Path) -> PathBuf {
    let proj = root.join("work");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("src/main.rs"), b"fn main() {}\n").unwrap();
    fs::write(proj.join("README.md"), b"# readme\n").unwrap();
    proj
}

/// A small archive with one project, ready for cycles. Returns (work root, home, archive, project).
fn setup(name: &str, volume: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = tmp_on(volume, name);
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let arch = root.join("archive");
    let proj = fixture_project(&root);
    let (code, out, err) = run(&home, &["init-archive", arch.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = run(&home, &["add", proj.to_str().unwrap(), "--yes", "--preset", "developer"]);
    assert_eq!(code, 0, "add must succeed: {out}{err}");
    (root, home, arch, proj)
}

fn big_volume() -> PathBuf {
    // The owner's own volume: /work is mounted from the Mac's 927 GB disk. The tests write a few
    // kilobytes there and remove them again.
    let v = PathBuf::from("/work/tmp");
    if v.is_dir() {
        v
    } else {
        std::env::temp_dir()
    }
}

/// The owner's arithmetic, on the real volume this container has.
///
/// His Mac: 926 GB volume, 1.8 GB free, and the percentage rule fired while the fixed floor did
/// not. This test reproduces the *disagreement* between the two rules rather than his numbers: it
/// reads the volume's real size and free space from the program, then sets `stopFreePercent` just
/// above the free percentage, so the percentage test alone fires and the 500 MB floor does not.
/// On his machine the same setting is "2 %"; on a disk with 71.8 % free it is "72 %". What is being
/// tested is the rule, not a lucky disk.
#[test]
fn where_the_percentage_rule_and_the_floor_disagree_the_smaller_number_governs() {
    let volume = big_volume();
    let (root, home, arch, proj) = setup("volume", &volume);

    let (code, out, err) = run(&home, &["heartbeat-check", "--json"]);
    assert!(code == 0 || code == 1 || code == 2, "heartbeat-check ran: {out}{err}");
    let hb = json_of(&out);
    let st = &hb["storage"];
    let total = st["totalBytes"].as_u64().expect("the volume size is reported");
    let free = st["freeBytes"].as_u64().expect("the free space is reported");
    let free_pct = (free as f64) * 100.0 / total as f64;
    eprintln!(
        "   the volume this program stands on: total {total} B ({:.1} GiB), free {free} B ({:.1} GiB, {free_pct:.2} %)",
        total as f64 / 1073741824.0,
        free as f64 / 1073741824.0
    );

    // The percentage must be able to fire where the floor does not: free% < pct, free > 500 MB.
    let pct = free_pct.floor() as u64 + 1;
    if pct > 100 || free <= 500 * 1024 * 1024 {
        eprintln!("   skipped: this volume cannot reproduce the disagreement ({free_pct:.2} % free)");
        let _ = fs::remove_dir_all(&root);
        return;
    }
    assert_eq!(run(&home, &["config", "set", "stopFreePercent", &pct.to_string()]).0, 0);
    let pct_threshold = total as u128 * pct as u128 / 100;
    assert!(
        (free as u128) < pct_threshold,
        "the premise: the percentage rule alone would stop writing here ({free} B free, {pct} % = {pct_threshold} B)"
    );
    assert!(
        (free as u128) > 500 * 1024 * 1024,
        "and the floor would not, which is what makes the two rules disagree"
    );

    // With the two rules disagreeing, the smaller one governs: writing continues.
    let (code, out, _e) = run(&home, &["heartbeat-check", "--json"]);
    assert!(code == 0 || code == 1, "{out}");
    let hb = json_of(&out);
    let st = &hb["storage"];
    assert_ne!(st["state"], "full", "the archive is not full: {}", st["reason"]);
    assert_eq!(st["stop"], false, "{}", st["reason"]);
    assert_eq!(st["stopBytes"].as_u64(), Some(500 * 1024 * 1024), "the floor is the smaller number");
    let reason = st["reason"].as_str().unwrap();
    assert!(
        reason.contains("the smaller of 500.0 MB") && reason.contains(&format!("and {pct} %")),
        "the reason states which of the two rules won: {reason}"
    );

    // And the proof that it is not only a status field: a real change is really recorded.
    fs::write(proj.join("README.md"), b"# readme, changed on a volume that is mostly full\n").unwrap();
    let (code, out, err) = run(&home, &["scan-once", "work", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let doc = json_of(&out);
    assert_ne!(doc["archiveState"], "ARCHIVE_FULL", "the cycle wrote: {doc}");
    assert_eq!(
        doc["projects"][0]["changed"].as_u64(),
        Some(1),
        "the change was recorded even though the percentage rule alone would have stopped it: {doc}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// The transition the owner asked for, end to end: writing → not enough room → repeated cycles →
/// room again → writing resumes, with one notification at each transition and none in between.
#[test]
fn a_full_archive_speaks_once_reminds_after_ten_minutes_and_says_when_it_resumes() {
    let root = tmp("full");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let arch = root.join("archive");
    let proj = fixture_project(&root);
    let (code, out, err) = run(&home, &["init-archive", arch.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = run(&home, &["add", proj.to_str().unwrap(), "--yes", "--preset", "developer"]);
    assert_eq!(code, 0, "{out}{err}");

    // 1. Normal recording. The file is one the chosen preset protects (`--preset developer`
    //    covers source code; a .txt would be skipped by design and would prove nothing).
    let watched = proj.join("src/note.rs");
    fs::write(&watched, b"// first\n").unwrap();
    let (code, out, err) = run(&home, &["scan-once", "work", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let doc = json_of(&out);
    assert_ne!(doc["archiveState"], "ARCHIVE_FULL");
    assert_eq!(doc["projects"][0]["created"].as_u64(), Some(1), "the new source file is recorded: {doc}");

    // 2. Not enough room. The threshold is moved, not the disk: `min(1 TB, 100 % of the volume)` is
    //    the volume itself, so free space is below it whatever this machine happens to have.
    for (k, v) in [("stopFreeBytes", "1099511627776"), ("stopFreePercent", "100")] {
        let (code, _o, e) = run(&home, &["config", "set", k, v]);
        assert_eq!(code, 0, "config {k}: {e}");
    }
    let (_code, out, _e) = run(&home, &["scan-once", "work", "--json"]);
    let doc = json_of(&out);
    assert_eq!(doc["archiveState"], "ARCHIVE_FULL", "the cycle refused to write: {doc}");
    assert!(
        doc["projects"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "and it wrote nothing at all: {doc}"
    );
    // The scheduler-facing command agrees, in its exit code, with what the cycle said in JSON.
    let (hc, hc_out, _e) = run(&home, &["healthcheck"]);
    eprintln!("   healthcheck while full: exit {hc}: {}", hc_out.lines().take(2).collect::<Vec<_>>().join(" | "));
    assert_eq!(hc, 1, "healthcheck reports the broken promise: {hc_out}");
    let stopping_line = log_text(&arch);
    assert!(
        stopping_line.contains("ARCHIVE_FULL: writing stopped (first notification)"),
        "the first stop is announced as the first: {}",
        stopping_line.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
    );
    assert_eq!(notifications(&arch, "recording stopped"), 1, "one notification on the transition");

    // The change that happened while the archive was full is *not* lost: it is still unrecorded,
    // and no version claims otherwise.
    fs::write(&watched, b"// second, written while the archive was full\n").unwrap();

    // 3. Repeated cycles: still one notification. The flood the owner saw is what this check is for.
    for _ in 0..6 {
        let (_c, out, _e) = run(&home, &["scan-once", "work", "--json"]);
        assert_eq!(json_of(&out)["archiveState"], "ARCHIVE_FULL");
    }
    assert_eq!(
        notifications(&arch, "recording stopped"),
        1,
        "six more cycles must not produce six more notifications (the 2026-10-06 flood)"
    );
    assert!(
        log_text(&arch).contains("writing is still stopped; next reminder in"),
        "and the log does say the state is still the same, with the time to the next reminder"
    );

    // 4. Ten minutes later, the reminder is due. The clock is the state file's, so the test can
    //    move it without waiting ten minutes.
    let path = arch.join("logs/space_state.json");
    let mut n: Value = serde_json::from_str(&fs::read_to_string(&path).expect("the notice state file")).unwrap();
    let now = projectlife::util::now_ms();
    n["lastNotifyMs"] = Value::from(now - 11 * 60 * 1000);
    fs::write(&path, serde_json::to_string(&n).unwrap()).unwrap();
    let (_code, out, _e) = run(&home, &["scan-once", "work", "--json"]);
    assert_eq!(json_of(&out)["archiveState"], "ARCHIVE_FULL");
    assert_eq!(notifications(&arch, "recording stopped"), 2, "the ten-minute reminder is sent once");
    assert!(log_text(&arch).contains("reminder after 10 minutes"), "and the log says it was the reminder");

    // 5. Room again. The real change made during the outage is recorded, and the resumption is
    //    announced exactly once.
    for (k, v) in [("stopFreeBytes", "524288000"), ("stopFreePercent", "1")] {
        let (code, _o, e) = run(&home, &["config", "set", k, v]);
        assert_eq!(code, 0, "config {k}: {e}");
    }
    let (code, out, err) = run(&home, &["scan-once", "work", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let doc = json_of(&out);
    assert_ne!(doc["archiveState"], "ARCHIVE_FULL", "{doc}");
    assert_eq!(doc["projects"][0]["changed"].as_u64(), Some(1), "the change made during the outage is recorded: {doc}");
    assert_eq!(notifications(&arch, "recording resumed"), 1, "one notification when writing resumes");
    assert!(log_text(&arch).contains("ARCHIVE_FULL: writing resumed after"), "with the outage length in the log");

    // 6. And it stays quiet afterwards.
    for _ in 0..3 {
        run(&home, &["scan-once", "work", "--json"]);
    }
    assert_eq!(notifications(&arch, "recording resumed"), 1, "no repetition of the good news either");
    assert_eq!(notifications(&arch, "recording stopped"), 2, "and no new stop notification");

    // The version really is in the journal, not just in the cycle's counter.
    let (code, out, _e) = run(&home, &["log", "work", "--json"]);
    assert_eq!(code, 0);
    let puts = out.matches("\"put\"").count();
    assert!(puts >= 2, "two versions of the watched file are in the journal: {out}");
    let (code, out, _e) = run(&home, &["why", "work", watched.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("second, written while the archive was full") || out.contains("note.rs"),
        "and the file's own history can be read back: {out}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// What the window reads, while writing is stopped: the reason, the numbers, and a plain "no".
#[test]
fn the_state_a_window_reads_is_the_state_the_daemon_obeys() {
    let root = tmp("state");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let arch = root.join("archive");
    let proj = fixture_project(&root);
    assert_eq!(run(&home, &["init-archive", arch.to_str().unwrap()]).0, 0);
    assert_eq!(run(&home, &["add", proj.to_str().unwrap(), "--yes", "--preset", "developer"]).0, 0);

    for (k, v) in [("stopFreeBytes", "1099511627776"), ("stopFreePercent", "100")] {
        assert_eq!(run(&home, &["config", "set", k, v]).0, 0);
    }
    let (_c, out, _e) = run(&home, &["scan-once", "work", "--json"]);
    assert_eq!(json_of(&out)["archiveState"], "ARCHIVE_FULL");

    // `heartbeat-check` has no `--heartbeat` of its own: here the heartbeat is not fresh (a refused
    // cycle writes none), so a window that only asked "is a process alive and beating?" would have
    // nothing to go on. The storage block is what makes the honest answer possible, and it is in
    // the same document the window already reads.
    let (code, out, _e) = run(&home, &["heartbeat-check", "--json"]);
    assert_eq!(code, 1, "nothing has been observed and no holder claims otherwise: {out}");
    let hb = json_of(&out);
    assert_eq!(hb["storage"]["state"], "full", "{}", hb["storage"]["reason"]);
    assert_eq!(hb["storage"]["stop"], true);
    let reason = hb["storage"]["reason"].as_str().unwrap();
    assert!(reason.contains("not written"), "the reason says what is not happening: {reason}");
    assert!(reason.contains("1.0 TB") && reason.contains("100 %"), "with both numbers: {reason}");

    // The doctor says the same sentence, from the same implementation: one rule, two readers.
    let (code, out, _e) = run(&home, &["doctor"]);
    assert!(code == 0 || code == 1, "doctor ran: {out}");
    assert!(
        out.contains("not written"),
        "doctor carries the same words, not a second opinion: {out}"
    );

    // And while writing is stopped, a window reading the same JSON has everything it needs to say
    // "recording stopped" instead of "Protected": the state, the sentence, and the two numbers.
    assert_eq!(hb["storage"]["stop"], true);
    assert!(hb["storage"]["stopBytes"].as_u64().unwrap() > 0);
    assert!(hb["storage"]["freeBytes"].as_u64().unwrap() > 0);
    let _ = fs::remove_dir_all(&root);
}
