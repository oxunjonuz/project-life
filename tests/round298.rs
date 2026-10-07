//! Round 298 — what the diagnostics say when nothing can be written.
//!
//! The owner's manual macOS test of 2026-10-06 reported, among three failures, that the daemon
//! "repeatedly logged ARCHIVE_FULL and did not create a heartbeat". The thresholds were wrong in
//! that build and are fixed (round 296), but the *reporting* half of that sentence survived into
//! today's build: an archive whose writing is stopped for want of room printed a heartbeat line
//! that blamed the scheduler, and offered a remedy (`pl scan-once --all`) that cannot work on a
//! full disk. These tests hold the words to the state.
//!
//! Writing is stopped here without touching any real disk: the stop threshold is raised until the
//! free space of whatever volume the test runs on is below it. That is what the rule does on a
//! genuinely full volume, and it makes the test independent of the machine it runs on.

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
    let p = std::env::temp_dir().join(format!("pl298-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(archive: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(core())
        .env("PROJECTLIFE_HOME", home)
        .arg("--archive")
        .arg(archive)
        .args(args)
        .output()
        .expect("run projectlife");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

struct Fixture {
    archive: PathBuf,
    home: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = tmp(name);
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("a.txt"), "first\n").unwrap();
        let f = Fixture { archive: root.join("archive"), home: root.join("home"), root };
        fs::create_dir_all(&f.home).unwrap();
        let (c, _, e) = run(&f.archive, &f.home, &["init-archive", f.archive.to_str().unwrap()]);
        assert_eq!(c, 0, "init-archive: {e}");
        let (c, o, e) = run(
            &f.archive,
            &f.home,
            &["add", work.to_str().unwrap(), "--name", "p", "--preset", "auto", "--yes"],
        );
        assert_eq!(c, 0, "add: {o}{e}");
        f
    }

    fn pl(&self, args: &[&str]) -> (i32, String, String) {
        run(&self.archive, &self.home, args)
    }

    /// Stop writing without filling a disk.
    ///
    /// Both numbers of the rule have to be raised, not just the fixed one: since round 296 the
    /// threshold is the *smaller* of the two, so a big fixed number on a big volume changes
    /// nothing — the first version of this helper did exactly that and stopped nothing, which is
    /// how the rule is meant to work. With both raised the threshold is 99 % of the volume, which
    /// is above the free space of any volume that is not already nearly full.
    fn stop_writing(&self) {
        for (k, v) in [("stopFreeBytes", "1000000000000000"), ("stopFreePercent", "99")] {
            let (c, o, e) = self.pl(&["config", "set", k, v]);
            assert_eq!(c, 0, "config set {k}: {o}{e}");
        }
    }

    /// Only warn: the fixed numbers go up, the percentages stay where they are, so the stop
    /// threshold (500 MB) is still far below the free space.
    fn warn_only(&self) {
        for (k, v) in [("warnFreeBytes", "1000000000000000"), ("warnFreePercent", "99")] {
            let (c, o, e) = self.pl(&["config", "set", k, v]);
            assert_eq!(c, 0, "config set {k}: {o}{e}");
        }
    }
}

/// 1. `heartbeat-check` — the cron recipe — must name the disk, not the scheduler.
#[test]
fn heartbeat_check_on_a_full_disk_names_the_disk_and_not_the_scheduler() {
    let f = Fixture::new("heartbeat-full");
    f.stop_writing();
    let (code, out, err) = f.pl(&["heartbeat-check"]);
    assert_eq!(code, 1, "nothing was ever observed, and nothing can be written: {out}{err}");
    assert!(out.contains("writing is stopped"), "the cause must be named first:\n{out}");
    assert!(out.contains("below the stop threshold"), "with the numbers:\n{out}");
    assert!(out.contains("free space on the volume holding"), "the remedy must be space:\n{out}");
    assert!(
        !out.contains("pl scan-once --all") && !out.contains("pl daemon start"),
        "a command that cannot write must not be offered as the fix:\n{out}"
    );
    // …and the same fact is in the machine-readable answer, where the window reads it.
    let (code, out, _e) = f.pl(&["heartbeat-check", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["storage"]["stop"], true, "{out}");
    assert!(
        v["storage"]["reason"].as_str().unwrap().contains("below the stop threshold"),
        "{out}"
    );
}

/// 2. The control: with room to write, the same command still says the ordinary thing. Without
/// this, the full-disk branch could be reached by a program that never takes the other one.
#[test]
fn heartbeat_check_with_room_still_advises_the_scheduler() {
    let f = Fixture::new("heartbeat-room");
    let (code, out, _e) = f.pl(&["heartbeat-check"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("pl scan-once --all") || out.contains("pl daemon status"),
        "with free space the ordinary remedies are the right ones:\n{out}"
    );
    assert!(!out.contains("writing is stopped"), "nothing is stopping the writing here:\n{out}");
}

/// 3. `healthcheck` — one code for a scheduler — must not report "nothing has been observed" as if
/// the cause were the schedule, while `doctor` in the same output says the disk is full.
#[test]
fn healthcheck_names_space_as_the_reason_nothing_is_recorded() {
    let f = Fixture::new("health-full");
    f.stop_writing();
    let (code, out, err) = f.pl(&["healthcheck"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        out.contains("nothing has ever been recorded") && out.contains("below the stop threshold"),
        "the heartbeat line must carry the cause:\n{out}"
    );
    assert!(
        out.contains("free space on the volume holding"),
        "and the remedy must be the one that can work:\n{out}"
    );
    assert!(
        !out.contains("pl daemon start") && !out.contains("pl scan-once --all"),
        "the remedies offered must not be unable to write:\n{out}"
    );
    assert!(
        out.contains("pl doctor prints the numbers"),
        "the scheduler's reader is pointed at the command that shows the arithmetic:\n{out}"
    );
}

/// 4. `doctor`'s remedy for a full archive has to be able to free space. It used to be the dry run
/// of a prune, which deletes nothing.
#[test]
fn doctor_offers_a_remedy_that_can_free_space() {
    let f = Fixture::new("doctor-full");
    f.stop_writing();
    let (code, out, err) = f.pl(&["doctor"]);
    assert_eq!(code, 1, "a stopped write is an error:\n{out}{err}");
    assert!(out.contains("stop threshold"), "the numbers are the diagnosis:\n{out}");
    let fix_line = out
        .lines()
        .find(|l| l.contains("fix:") && l.contains("prune"))
        .unwrap_or_else(|| panic!("a remedy that frees space:\n{out}"));
    assert!(
        fix_line.contains("without --dry-run"),
        "the remedy must say that the deletion is what frees the space: {fix_line}"
    );
    // The dry run is still suggested while there is room: it is the safe way to look.
    let healthy = Fixture::new("doctor-warn");
    healthy.warn_only();
    let (code, out, _e) = healthy.pl(&["doctor"]);
    assert_eq!(code, 0, "a warning is not an error:\n{out}");
    assert!(out.contains("--dry-run"), "looking before deleting stays the first step:\n{out}");
}

/// 5. Nothing is lost by stopping: the state does not advance while nothing can be stored, so the
/// pass that runs once there is room again records the change. This is the claim the remedy makes
/// ("nothing is lost"), so it is measured rather than asserted in prose.
#[test]
fn a_change_made_while_writing_was_stopped_is_recorded_when_there_is_room() {
    let f = Fixture::new("nothing-lost");
    let live = f.root.join("work/a.txt");
    f.stop_writing();
    fs::write(&live, "second\n").unwrap();
    let (code, out, _e) = f.pl(&["scan-once", "--all"]);
    assert_eq!(code, 1, "the pass refuses to write while the archive is full:\n{out}");
    assert!(out.contains("ARCHIVE_FULL"), "{out}");
    // Room returns.
    for (k, v) in [("stopFreeBytes", "524288000"), ("stopFreePercent", "1")] {
        let (c, o, e) = f.pl(&["config", "set", k, v]);
        assert_eq!(c, 0, "config set {k}: {o}{e}");
    }
    let (c, o, e) = f.pl(&["scan-once", "--all"]);
    assert_eq!(c, 0, "the pass must record what happened while it could not: {o}{e}");
    let (c, out, _e) = f.pl(&["log", "p"]);
    assert_eq!(c, 0);
    assert!(
        out.contains("put") || out.contains("version"),
        "the change made during the stop is in the journal:\n{out}"
    );
    let (c, out, _e) = f.pl(&["last-good", "p"]);
    assert_eq!(c, 0, "{out}");
}
