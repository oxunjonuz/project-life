//! Round 300 — three things the research said people need and the program did not have:
//!
//! 1. `restore --missing`: give back the files an agent deleted, and touch nothing else.
//! 2. FR-CFG-3: a configuration change that applies to a running observation, without a restart.
//! 3. The notification ledger: what the program told you, in a file you can still read tomorrow.
//!
//! Everything here runs the shipped binary or the library on a real filesystem. The one thing no
//! test can do is run the window: that is `tools/ui_e2e.py` and the app's own suite.

use projectlife::archive::Archive;
use projectlife::daemon;
use projectlife::restore::{self, RestoreOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn core() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    let candidate = p.join("projectlife");
    if candidate.exists() {
        return candidate;
    }
    PathBuf::from(env!("CARGO_BIN_EXE_projectlife"))
}

fn tmp(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("pl300-{name}-{}-{id}", std::process::id()));
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

fn json_of(out: &str) -> serde_json::Value {
    let lines: Vec<&str> = out.lines().collect();
    for start in 0..lines.len().min(8) {
        let chunk = lines[start..].join("\n");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&chunk) {
            return v;
        }
    }
    panic!("no JSON document in the output:\n{out}");
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    archive: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A project with three text files, already observed once, plus an archive and a HOME.
    fn new(name: &str) -> Fixture {
        let root = tmp(name);
        let work = root.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("a.txt"), "alpha\n").unwrap();
        fs::write(work.join("src/b.txt"), "beta\n").unwrap();
        fs::write(work.join("src/c.txt"), "gamma\n").unwrap();
        let f = Fixture { work, archive: root.join("archive"), home: root.join("home"), root };
        fs::create_dir_all(&f.home).unwrap();
        let (c, _, e) = run(&f.archive, &f.home, &["init-archive", f.archive.to_str().unwrap()]);
        assert_eq!(c, 0, "init-archive: {e}");
        let (c, _, e) = run(
            &f.archive,
            &f.home,
            &["config", "set", "stopFreePercent", "0"],
        );
        assert_eq!(c, 0, "config set: {e}");
        let (c, _, e) = run(
            &f.archive,
            &f.home,
            &["config", "set", "warnFreePercent", "0"],
        );
        assert_eq!(c, 0, "config set: {e}");
        let (c, o, e) = run(
            &f.archive,
            &f.home,
            &["add", f.work.to_str().unwrap(), "--profile", "all", "--yes"],
        );
        assert_eq!(c, 0, "add: {o}{e}");
        f
    }

    fn log(&self) -> String {
        fs::read_to_string(self.archive.join("logs/projectlife.log")).unwrap_or_default()
    }

    fn ledger(&self) -> String {
        fs::read_to_string(self.archive.join("logs/notifications.jsonl")).unwrap_or_default()
    }

    fn at_now(&self) -> String {
        let out = Command::new(core())
            .env("PROJECTLIFE_HOME", &self.home)
            .arg("--archive")
            .arg(&self.archive)
            .args(["status", "work", "--json"])
            .output()
            .unwrap();
        let v = json_of(&String::from_utf8_lossy(&out.stdout));
        v.as_array()
            .and_then(|a| a.first())
            .and_then(|p| p.get("lastObservedAt"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .expect("status must report when the project was last observed")
    }
}

/// A daemon started by a test, killed when the test ends — including when it panics.
///
/// Round 300's first draft killed the daemon at the end of the happy path only, and the campaign left
/// nine of them running against fixtures that had already been deleted. A test that leaks a process
/// when it fails is a test that damages the machine it is measuring.
struct DaemonGuard(std::process::Child);

impl DaemonGuard {
    fn start(f: &Fixture) -> DaemonGuard {
        let child = Command::new(core())
            .env("PROJECTLIFE_HOME", &f.home)
            .arg("--archive")
            .arg(&f.archive)
            .args(["daemon", "run"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the daemon must start");
        DaemonGuard(child)
    }

    fn wait_for_log(&self, f: &Fixture, needle: &str, ticks: usize) -> bool {
        for _ in 0..ticks {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if f.log().contains(needle) {
                return true;
            }
        }
        false
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn stop(mut self) {
        let _ = Command::new("kill").arg("-TERM").arg(self.pid().to_string()).status();
        for _ in 0..100 {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.0.kill();
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = Command::new("kill").arg("-TERM").arg(self.pid().to_string()).status();
        for _ in 0..60 {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.0.kill();
    }
}

// =============================================================================================
// 1. `restore --missing`
// =============================================================================================

/// The scenario from the research: an agent deleted files, and the person has since edited one of
/// them by hand. The repair must bring back exactly what is gone and leave the hand work intact.
#[test]
fn a_repair_puts_back_only_what_is_missing_and_leaves_everything_else_alone() {
    let f = Fixture::new("repair-basic");
    let moment = f.at_now();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // The agent: two files deleted, one of them re-created by hand with different contents.
    fs::remove_file(f.work.join("src/b.txt")).unwrap();
    fs::remove_file(f.work.join("src/c.txt")).unwrap();
    fs::write(f.work.join("src/c.txt"), "GAMMA BY HAND\n").unwrap();
    fs::write(f.work.join("a.txt"), "alpha edited by hand\n").unwrap();

    let (c, o, e) = run(
        &f.archive,
        &f.home,
        &["restore", "work", "--at", &moment, "--into-project", "--missing", "--preview", "--json"],
    );
    assert_eq!(c, 0, "preview: {o}{e}");
    let plan = json_of(&o);
    assert_eq!(plan["create"], 1, "exactly one file is gone: {plan}");
    assert_eq!(plan["overwrite"], 0, "a repair plans no overwrite: {plan}");
    assert_eq!(plan["present"], 2, "two files exist and are left alone: {plan}");
    assert_eq!(plan["deleteExtra"], 0, "a repair plans no deletion: {plan}");

    let (c, o, e) = run(
        &f.archive,
        &f.home,
        &["restore", "work", "--at", &moment, "--into-project", "--missing", "--yes", "--json"],
    );
    assert_eq!(c, 0, "repair: {o}{e}");
    let rep = json_of(&o);
    assert_eq!(rep["restored"], 1, "{rep}");
    assert_eq!(
        rep["restoredPaths"][0], "src/b.txt",
        "the file that was actually missing: {rep}"
    );
    assert_eq!(fs::read_to_string(f.work.join("src/b.txt")).unwrap(), "beta\n");
    assert_eq!(
        fs::read_to_string(f.work.join("src/c.txt")).unwrap(),
        "GAMMA BY HAND\n",
        "the file the person wrote by hand must not be replaced by the archived version"
    );
    assert_eq!(
        fs::read_to_string(f.work.join("a.txt")).unwrap(),
        "alpha edited by hand\n",
        "an edited file must not be reverted by a repair"
    );
}

/// The window between the plan and the write is where a careless implementation overwrites: the plan
/// said "empty", and then the person (or an agent) created the file. The rule is create-only, so the
/// file that exists now wins — including at write time, not only at plan time.
#[test]
fn a_file_that_appears_between_the_plan_and_the_write_is_never_overwritten() {
    let f = Fixture::new("repair-race");
    let moment = f.at_now();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::remove_file(f.work.join("src/b.txt")).unwrap();

    let arch = Archive::open(&f.archive).unwrap();
    let project = arch.find("work").unwrap();
    let at = projectlife::util::parse_at(&moment, projectlife::util::now_ms()).unwrap();
    let opts = RestoreOptions {
        at,
        at_label: "test".into(),
        paths: Vec::new(),
        to: None,
        into_project: true,
        clean: false,
        preview: false,
        missing: true,
    };
    let plan = restore::plan(&arch, &project, &opts).unwrap();
    assert_eq!(plan.create, 1, "the plan sees one missing file");
    // Between the plan and the write, the file comes back on its own with different contents.
    fs::write(f.work.join("src/b.txt"), "written after the plan\n").unwrap();
    let report = restore::execute(&project, &plan, &opts).unwrap();
    assert_eq!(report.restored, 0, "nothing may be written over the file that appeared");
    assert_eq!(report.skipped_present, 1, "and the skip must be counted, not silent");
    assert_eq!(
        fs::read_to_string(f.work.join("src/b.txt")).unwrap(),
        "written after the plan\n"
    );
}

/// The guard exists in the library, not only behind the command line: a caller that builds
/// `RestoreOptions` itself must be refused too, and a plan must say which mode built it. Round 300's
/// first campaign left this untested — two faults survived, and both were the tests' fault.
#[test]
fn a_plan_says_which_mode_built_it_and_refuses_the_two_flags_that_disagree() {
    let f = Fixture::new("repair-plan-mode");
    let moment = f.at_now();
    let arch = Archive::open(&f.archive).unwrap();
    let project = arch.find("work").unwrap();
    let at = projectlife::util::parse_at(&moment, projectlife::util::now_ms()).unwrap();
    let make = |missing: bool, clean: bool| RestoreOptions {
        at,
        at_label: "test".into(),
        paths: Vec::new(),
        to: Some(f.root.join(if missing { "one" } else { "two" })),
        into_project: false,
        clean,
        preview: true,
        missing,
    };
    // A caller that asks for a repair and a deletion in one breath is refused by the library.
    let err = restore::plan(&arch, &project, &make(true, true)).unwrap_err();
    assert!(
        err.contains("--missing only creates") && err.contains("--clean deletes"),
        "the library must refuse it by name, not silently pick one: {err}"
    );
    // And the plan tells its reader which of the two it is — in both directions, so a constant
    // cannot satisfy this.
    assert!(restore::plan(&arch, &project, &make(true, false)).unwrap().missing, "a repair plan must say it is one");
    assert!(
        !restore::plan(&arch, &project, &make(false, false)).unwrap().missing,
        "an ordinary restore plan must not claim to be a repair"
    );
}

#[test]
fn a_repair_refuses_to_delete_and_says_which_two_flags_disagree() {
    let f = Fixture::new("repair-refuse");
    let moment = f.at_now();
    let (c, o, e) = run(
        &f.archive,
        &f.home,
        &["restore", "work", "--at", &moment, "--into-project", "--missing", "--clean", "--yes"],
    );
    assert_ne!(c, 0, "the two flags together must be refused: {o}");
    assert!(
        e.contains("--missing only creates") && e.contains("--clean deletes"),
        "the refusal must name both flags and what each does: {e}"
    );
}

#[test]
fn a_repair_may_fill_a_folder_that_already_exists() {
    let f = Fixture::new("repair-folder");
    let moment = f.at_now();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::remove_file(f.work.join("src/b.txt")).unwrap();
    // The target is an existing folder with unrelated content: a repair fills it, it does not insist
    // on a fresh folder the way a full restore does.
    let out_dir = f.root.join("partial");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("unrelated.txt"), "keep me\n").unwrap();
    let project_before = fs::read_to_string(f.work.join("a.txt")).unwrap();
    let (c, o, e) = run(
        &f.archive,
        &f.home,
        &[
            "restore",
            "work",
            "--at",
            &moment,
            "--missing",
            "--to",
            out_dir.to_str().unwrap(),
            "--yes",
            "--json",
        ],
    );
    assert_eq!(c, 0, "repair into an existing folder: {o}{e}");
    let rep = json_of(&o);
    // Nothing of the project was ever written into this folder, so all three files are missing there
    // and all three are created — that is what repair means in a target that is not the project.
    assert_eq!(rep["restored"], 3, "{rep}");
    assert_eq!(rep["deleted"], 0, "and nothing is deleted: {rep}");
    assert_eq!(fs::read_to_string(out_dir.join("src/b.txt")).unwrap(), "beta\n");
    assert_eq!(
        fs::read_to_string(out_dir.join("unrelated.txt")).unwrap(),
        "keep me\n",
        "the repair must not touch what was already in the target folder"
    );
    assert_eq!(
        fs::read_to_string(f.work.join("a.txt")).unwrap(),
        project_before,
        "a repair into another folder must not write into the project"
    );
    assert!(
        !f.work.join("src/b.txt").exists(),
        "and it must not put the file back where it came from either"
    );
}

/// `--missing` creates. It cannot delete, even in the mode where deletion is otherwise allowed:
/// a file that appeared after the moment is simply not part of the plan.
#[test]
fn a_repair_never_deletes_a_file_that_appeared_after_the_moment() {
    let f = Fixture::new("repair-nodelete");
    let moment = f.at_now();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(f.work.join("brand-new.txt"), "created after the moment\n").unwrap();
    let (c, o, e) = run(
        &f.archive,
        &f.home,
        &["restore", "work", "--at", &moment, "--into-project", "--missing", "--yes", "--json"],
    );
    assert_eq!(c, 0, "repair: {o}{e}");
    assert_eq!(
        fs::read_to_string(f.work.join("brand-new.txt")).unwrap(),
        "created after the moment\n"
    );
    assert!(
        !o.contains("\"deleted\": 1"),
        "a repair must not delete anything: {o}"
    );
}

// =============================================================================================
// 2. The notification ledger
// =============================================================================================

/// The mass-deletion notification is the one that matters most, so the test drives it end to end:
/// wipe the project, observe, and read the ledger back — including the command that repairs it.
#[test]
fn a_mass_deletion_leaves_a_readable_line_with_the_project_and_the_repair_command() {
    let f = Fixture::new("ledger-mass");
    // Enough files that the mass thresholds are passed on their own (minMassFiles=3, percent=30).
    for i in 0..20 {
        fs::write(f.work.join("src").join(format!("f{i:02}.txt")), format!("body {i}\n")).unwrap();
    }
    let (c, _, e) = run(&f.archive, &f.home, &["scan-once", "work"]);
    assert_eq!(c, 0, "scan: {e}");
    for i in 0..20 {
        fs::remove_file(f.work.join("src").join(format!("f{i:02}.txt"))).unwrap();
    }
    let (c, o, e) = run(&f.archive, &f.home, &["scan-once", "work"]);
    assert_eq!(c, 0, "scan after the wipe: {o}{e}");
    assert!(o.contains("-23") || o.contains("-2"), "the wipe must be observed: {o}");

    let (c, o, e) = run(&f.archive, &f.home, &["notifications", "--json"]);
    assert_eq!(c, 0, "notifications: {o}{e}");
    let v = json_of(&o);
    let rows = v["notifications"].as_array().cloned().unwrap_or_default();
    let mass = rows
        .iter()
        .find(|r| r["kind"] == "mass")
        .unwrap_or_else(|| panic!("a mass change must leave a line: {v}"));
    assert_eq!(mass["project"], "work", "the line must name the project: {mass}");
    let body = mass["body"].as_str().unwrap_or("");
    assert!(
        body.contains("--missing --into-project"),
        "the line must carry the repair command, not only the full restore: {body}"
    );
    assert!(
        mass["at"].as_i64().unwrap_or(0) > 0 && mass["atIso"].as_str().unwrap_or("").contains('T'),
        "the line must be stamped with a moment a person can paste back: {mass}"
    );
    // And the same file is a real JSONL file: one object per line, no prose.
    for line in f.ledger().lines().filter(|l| !l.trim().is_empty()) {
        let _: serde_json::Value = serde_json::from_str(line).expect("every ledger line must be JSON");
    }
}

#[test]
fn the_ledger_is_what_the_core_reads_back_and_the_limit_is_respected() {
    let f = Fixture::new("ledger-limit");
    for _ in 0..5 {
        let (c, _, e) = run(&f.archive, &f.home, &["notify", "test"]);
        assert_eq!(c, 0, "notify test: {e}");
    }
    let arch = Archive::open(&f.archive).unwrap();
    let all = daemon::read_notifications(&arch, 100);
    assert_eq!(all.len(), 5, "five messages were raised");
    let two = daemon::read_notifications(&arch, 2);
    assert_eq!(two.len(), 2, "the limit is a tail, not a filter");
    assert_eq!(
        two[1]["at"], all[4]["at"],
        "the newest line is last, the way the file appends"
    );
    let (c, o, _) = run(&f.archive, &f.home, &["notifications", "--kind", "test", "--limit", "3", "--json"]);
    assert_eq!(c, 0);
    assert_eq!(json_of(&o)["count"], 3);
    let (c, o, _) = run(&f.archive, &f.home, &["notifications", "--kind", "mass", "--json"]);
    assert_eq!(c, 0);
    assert_eq!(
        json_of(&o)["count"],
        0,
        "a filter that matches nothing must say zero, not fall back to everything"
    );
}

/// A half-written line must not make the ledger unreadable: the reader skips what it cannot parse and
/// keeps the rest, the same rule the journal uses.
#[test]
fn a_damaged_line_in_the_ledger_does_not_hide_the_lines_around_it() {
    let f = Fixture::new("ledger-damaged");
    let (c, _, _) = run(&f.archive, &f.home, &["notify", "test"]);
    assert_eq!(c, 0);
    let ledger = f.archive.join("logs/notifications.jsonl");
    let mut text = fs::read_to_string(&ledger).unwrap();
    text.push_str("{\"at\": 1, \"kind\": \"mass\", \"tit"); // a line cut in half, no newline
    fs::write(&ledger, text).unwrap();
    let arch = Archive::open(&f.archive).unwrap();
    let rows = daemon::read_notifications(&arch, 50);
    assert_eq!(rows.len(), 1, "the intact line survives, the fragment is dropped: {rows:?}");
    assert_eq!(rows[0]["kind"], "test");
}

#[test]
fn a_silent_run_is_not_reported_as_a_message() {
    let f = Fixture::new("ledger-silent");
    // Nothing has happened: no pass has even been run beyond `add`.
    let (c, o, _) = run(&f.archive, &f.home, &["notifications", "--json"]);
    assert_eq!(c, 0);
    let v = json_of(&o);
    assert_eq!(v["count"], 0, "an empty ledger is reported as empty: {v}");
    assert!(v["notifications"].as_array().unwrap().is_empty());
}

// =============================================================================================
// 3. FR-CFG-3 — a configuration change that applies without a restart
// =============================================================================================

#[test]
fn a_configuration_file_that_changed_is_re_read_and_the_changed_keys_are_named() {
    let f = Fixture::new("cfg-reload");
    let mut arch = Archive::open(&f.archive).unwrap();
    assert_eq!(
        arch.reload_config(),
        None,
        "nothing changed, so nothing may be reported as changed"
    );
    let cfg_path = f.archive.join("config.json");
    let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
    v["intervalSeconds"] = serde_json::Value::from(1);
    v["notifications"] = serde_json::Value::from(true);
    fs::write(&cfg_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let changed = arch.reload_config().expect("the changed file must be noticed");
    assert_eq!(changed, vec!["intervalSeconds".to_string()], "only the key that differs: {changed:?}");
    assert_eq!(arch.config.i64_of("intervalSeconds", 5), 1, "and the value is in force");
    assert_eq!(arch.reload_config(), None, "a second read of the same bytes changes nothing");
}

/// The bytes changed but no value did: that is not a change, and the program says so instead of
/// claiming to have applied something.
#[test]
fn a_rewritten_file_with_the_same_values_is_reported_as_nothing_to_apply() {
    let f = Fixture::new("cfg-same");
    let mut arch = Archive::open(&f.archive).unwrap();
    let cfg_path = f.archive.join("config.json");
    let text = fs::read_to_string(&cfg_path).unwrap();
    // Same object, different bytes: different key order and indentation.
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys.reverse();
    let mut reordered = serde_json::Map::new();
    for k in keys {
        reordered.insert(k.clone(), v[&k].clone());
    }
    fs::write(&cfg_path, serde_json::to_string(&serde_json::Value::Object(reordered)).unwrap()).unwrap();
    let changed = arch.reload_config().expect("the bytes did change");
    assert!(changed.is_empty(), "no key's value differs: {changed:?}");
    assert!(
        f.log().contains("no key's value differs"),
        "the log must say why nothing was applied"
    );
}

/// The end-to-end version: a real daemon, a real edit of the file while it runs, and a real SIGHUP.
/// The proof is not that a flag is set somewhere but that the interval in force changes: the daemon
/// logs both the old and the new value, and writes the record where another process can read it.
#[test]
fn a_running_daemon_applies_a_configuration_change_without_a_restart() {
    let f = Fixture::new("cfg-daemon");
    let daemon = DaemonGuard::start(&f);
    assert!(
        daemon.wait_for_log(&f, "daemon started", 100),
        "the daemon never logged its start"
    );

    let cfg_path = f.archive.join("config.json");
    let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
    v["intervalSeconds"] = serde_json::Value::from(1);
    fs::write(&cfg_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let mut applied = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let log = f.log();
        if log.contains("configuration re-read without a restart") && log.contains("5000 ms -> 1000 ms") {
            applied = true;
            break;
        }
    }
    assert!(applied, "the running daemon did not apply the change:\n{}", f.log());

    // SIGHUP with an unchanged file must be answered with the truth, not with a fake re-apply.
    let _ = Command::new("kill").arg("-HUP").arg(daemon.pid().to_string()).status();
    let mut answered = false;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if f.log().contains("SIGHUP: the configuration file is unchanged") {
            answered = true;
            break;
        }
    }
    assert!(answered, "SIGHUP was not answered:\n{}", f.log());

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.archive.join("config_state.json")).unwrap()).unwrap();
    assert_eq!(state["reloads"], 1, "one re-read, recorded for another process to read: {state}");
    assert_eq!(state["intervalSeconds"], 1, "{state}");
    assert_eq!(state["changedKeys"][0], "intervalSeconds", "{state}");

    daemon.stop();
    assert!(
        f.log().contains("daemon stopped on signal"),
        "the daemon must stop cleanly:\n{}",
        f.log()
    );
}

/// Turning the filesystem-notification trigger off must not need a restart either, and the counters
/// that describe what happened before the change must survive it.
#[test]
fn the_notification_trigger_can_be_turned_off_while_the_daemon_runs() {
    let f = Fixture::new("cfg-trigger");
    let daemon = DaemonGuard::start(&f);
    assert!(daemon.wait_for_log(&f, "daemon started", 100), "the daemon never started");
    let (c2, _, e) = run(&f.archive, &f.home, &["config", "set", "watchTriggers", "false"]);
    assert_eq!(c2, 0, "config set: {e}");
    let mut off = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if f.log().contains("trigger: turned off without a restart") {
            off = true;
            break;
        }
    }
    assert!(off, "the trigger was not turned off live:\n{}", f.log());
    daemon.stop();
    let st = daemon::trigger_state(&Archive::open(&f.archive).unwrap());
    assert!(st.is_some(), "the trigger state must still be written after the change");
}
