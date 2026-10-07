//! Round 295 — the two things the desktop app needed from the core, tested from outside it.
//!
//! 1. `pl add --dry-run [--json]`: the app shows what *would* be protected before it creates
//!    anything. The point of the flag is that it writes nothing at all, so the test checks the
//!    archive byte for byte before and after.
//! 2. `--at` accepts the ISO-8601 UTC form the program itself prints (`pl log --json`, `pl tree`,
//!    `pl why`). Handing a moment back to the program that printed it must work.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn core() -> PathBuf {
    // The test binary sits in target/<profile>/deps/; the program is two levels up.
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
    let p = std::env::temp_dir().join(format!("pl295-{}-{}", name, std::process::id()));
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

/// Every file in a tree with its size and content hash — so "nothing was written" is a measurement.
fn snapshot(root: &Path) -> Vec<(String, u64, String)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, u64, String)>) {
        let mut entries: Vec<_> = match fs::read_dir(dir) {
            Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
            Err(_) => return,
        };
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
            let md = match fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                out.push((format!("{rel}/"), 0, String::new()));
                walk(&p, base, out);
            } else {
                let body = fs::read(&p).unwrap_or_default();
                let sum = {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(&body);
                    format!("{:x}", h.finalize())
                };
                out.push((rel, md.len(), sum));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

fn fixture_project(root: &Path) -> PathBuf {
    let proj = root.join("work");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::create_dir_all(proj.join("node_modules")).unwrap();
    fs::write(proj.join("src/main.rs"), b"fn main() {}\n").unwrap();
    fs::write(proj.join("README.md"), b"# readme\n").unwrap();
    fs::write(proj.join(".env"), b"SECRET=1\n").unwrap();
    fs::write(proj.join("node_modules/x.js"), b"module.exports = 1;\n").unwrap();
    fs::write(proj.join("movie.mp4"), b"\x00\x01fake").unwrap();
    proj
}

#[test]
fn dry_run_reports_the_estimate_and_writes_nothing_at_all() {
    let root = tmp("dryrun");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let arch = root.join("archive");
    let proj = fixture_project(&root);

    let (code, out, err) = run(&home, &["init-archive", arch.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    let before_archive = snapshot(&arch);
    let before_project = snapshot(&proj);

    let (code, out, err) = run(
        &home,
        &["add", proj.to_str().unwrap(), "--preset", "developer", "--dry-run", "--json"],
    );
    assert_eq!(code, 0, "dry run must succeed without --yes: {out}{err}");

    // The JSON document is the last one on stdout, after the detection report.
    let json_start = out.rfind("\n{").map(|i| i + 1).expect("a JSON document in the output");
    let doc: serde_json::Value = serde_json::from_str(&out[json_start..]).expect("the output ends with JSON");
    assert_eq!(doc["created"], serde_json::Value::Bool(false));
    assert!(doc["files"].as_u64().unwrap() >= 2, "the estimate names the files: {doc}");
    assert!(doc["bytes"].as_u64().unwrap() > 0);
    let skipped = doc["skippedByReason"].as_object().unwrap();
    assert!(skipped.contains_key("secret"), "the secret is named as skipped: {doc}");
    assert!(skipped.contains_key("ignored_dir"), "node_modules is named as skipped: {doc}");
    assert!(doc["preset"]["id"].as_str().is_some());

    // Nothing at all: no project, no journal, no blob, no config change, no touched project file.
    let after_archive = snapshot(&arch);
    assert_eq!(before_archive, after_archive, "a dry run changed the archive");
    assert_eq!(before_project, snapshot(&proj), "a dry run changed the project folder");
    assert_eq!(fs::read_dir(arch.join("projects")).unwrap().count(), 0, "a project was created");
    assert!(!arch.join(".lock").exists(), "a lock file was left behind");

    // And the real add afterwards still works from the same state.
    let (code, out, err) = run(&home, &["add", proj.to_str().unwrap(), "--preset", "developer", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, _e) = run(&home, &["status", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("work"), "the project is there after a real add: {out}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn at_accepts_the_iso_moment_the_program_itself_prints() {
    let root = tmp("iso");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let arch = root.join("archive");
    let proj = fixture_project(&root);

    assert_eq!(run(&home, &["init-archive", arch.to_str().unwrap()]).0, 0);
    let (code, out, err) = run(&home, &["add", proj.to_str().unwrap(), "--preset", "developer", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");

    // Take a moment from the program's own output and hand it straight back to --at.
    let (code, out, _e) = run(&home, &["log", "work", "--json"]);
    assert_eq!(code, 0);
    let events: Vec<serde_json::Value> = serde_json::from_str(&out).expect("log --json is a JSON array");
    let first_ts = events
        .iter()
        .filter_map(|e| e.get("ts").and_then(|v| v.as_i64()))
        .next()
        .expect("at least one event");
    let iso = ms_to_iso(first_ts);

    for cmd in [vec!["tree", "work", "--at", iso.as_str(), "--json"], vec!["why", "work", "--at", iso.as_str()]] {
        let (code, out, err) = run(&home, &cmd);
        assert_eq!(code, 0, "`{}` refused the program's own moment {iso}: {out}{err}", cmd.join(" "));
    }

    // A date without a time still works, and a nonsense moment still fails: the new path is not a
    // licence to accept anything.
    //
    // The moment below is the archive's own first event, written the way a person writes it — not a
    // fixed string. A hard-coded "2026-10-06 12:00" was here first, and it made this test depend on
    // the wall clock: before noon it named a moment in the future (accepted), after noon a moment
    // before the archive existed (refused). It was red at 13:48 and green at 11:00 on the same day.
    //
    // It is the *newest* event, not the oldest: the program refuses a moment earlier than the start
    // of the history it holds, and writing a moment as a plain local date-time drops the
    // milliseconds, so the oldest event could land a fraction of a second before the boundary.
    //  Make the history span more than a second first: a moment written the way a person writes it
    //  drops the milliseconds, so a boundary that is only a fraction of a second wide cannot hold
    //  one. (The first version of this check used a fixed calendar string, which made the test
    //  depend on the wall clock: green at 11:00, red at 13:48 on the same day.)
    std::thread::sleep(std::time::Duration::from_millis(1200));
    fs::write(proj.join("README.md"), b"# readme, second draft\n").unwrap();
    let (code, o, e) = run(&home, &["scan-once", "work"]);
    assert_eq!(code, 0, "one observation pass: {o}{e}");
    let (code, out, _e) = run(&home, &["log", "work", "--json"]);
    assert_eq!(code, 0);
    let events: Vec<serde_json::Value> = serde_json::from_str(&out).expect("log --json");
    let last_ts = events
        .iter()
        .filter_map(|e| e.get("ts").and_then(|v| v.as_i64()))
        .max()
        .expect("at least one event");
    assert!(last_ts > first_ts + 1000, "the fixture must span more than a second ({first_ts} -> {last_ts})");
    let plain = projectlife::util::fmt_local(last_ts);
    let (code, o, e) = run(&home, &["tree", "work", "--at", plain.as_str(), "--json"]);
    assert_eq!(code, 0, "a plain local date-time must keep working: {plain} {o}{e}");
    let (code, _o, _e) = run(&home, &["tree", "work", "--at", "not-a-time", "--json"]);
    assert_ne!(code, 0, "a nonsense moment must still be refused");
    let _ = fs::remove_dir_all(&root);
}

fn ms_to_iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        (rem % 3_600_000) / 60_000,
        (rem % 60_000) / 1000,
        rem % 1000
    )
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}
