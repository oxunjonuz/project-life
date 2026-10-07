//! Round 299 — the menu, and one sentence it found.
//!
//! The round's own subject (a full menu over the core's functions) lives in the desktop app crate
//! and is held by `tools/menu_contract_check.py`, which drives the real server and runs every entry.
//! What belongs here is the half that is the core's: a message the menu work made me read.
//!
//! A restore asked for a moment 900 ms before the start of the history answered
//! "moment 2026-10-07 08:31:26 is earlier than the start of the available history 2026-10-07 08:31:26"
//! — the same second printed twice, because the comparison is at millisecond precision and the
//! sentence was not. An error message that reads as nonsense is worse than a terse one: it moves the
//! reader's attention to the wrong thing. The boundary is still the boundary; the words now carry the
//! precision they are comparing.

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
    let p = std::env::temp_dir().join(format!("pl299-{}-{}", name, std::process::id()));
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

/// An archive with a project whose whole history falls inside one second — the case that produced the
/// unreadable message. A moment 1 ms before the start must be refused, and the refusal must print two
/// different strings.
#[test]
fn a_boundary_a_millisecond_away_is_reported_with_the_precision_it_was_compared_at() {
    let root = tmp("boundary");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("a.txt"), "hello\n").unwrap();
    let archive = root.join("archive");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let (c, _o, e) = run(&archive, &home, &["init-archive", archive.to_str().unwrap()]);
    assert_eq!(c, 0, "init-archive: {e}");
    let (c, _o, e) = run(
        &archive,
        &home,
        &["add", work.to_str().unwrap(), "--name", "p", "--preset", "auto", "--yes"],
    );
    assert_eq!(c, 0, "add: {e}");

    // The boundary the core itself keeps: `historyStartsAt`, written into the project record. Taking
    // it from the journal would be one millisecond off — the first *event* is written after the
    // moment the history is defined to start at, and that is exactly the sliver this test is about.
    let projects = archive.join("projects");
    let mut hstart = 0i64;
    for entry in fs::read_dir(&projects).expect("projects dir") {
        let dir = entry.unwrap().path();
        let rec = fs::read_to_string(dir.join("project.json")).unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&rec) {
            if let Some(s) = v.get("historyStartsAt").and_then(|s| s.as_str()) {
                hstart = hstart.max(projectlife::archive::parse_iso_ms(s).unwrap_or(0));
            }
        }
    }
    assert!(hstart > 0, "the fixture records no history start");
    let earliest = hstart;

    let iso = |ms: i64| {
        // The same shape the window sends: ISO UTC with milliseconds.
        let secs = ms.div_euclid(1000);
        let milli = ms.rem_euclid(1000);
        let (y, mo, d, h, mi, s) = civil_from_ms(secs);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
    };

    // 1 ms before the start: refused, and the two moments are distinguishable in the text.
    let (c, _o, e) = run(
        &archive,
        &home,
        &["restore", "p", "--at", &iso(earliest - 1), "--preview"],
    );
    assert_ne!(c, 0, "a moment before the history starts must be refused");
    let line = e.lines().next().unwrap_or("").to_string();
    let (asked, start) = two_moments(&line);
    assert!(
        asked.contains('.') || start.contains('.'),
        "the sentence must carry the precision it compared at: {line}"
    );
    assert_ne!(asked, start, "the same string twice is what made this message unreadable: {line}");

    // Exactly the start: accepted — the boundary itself belongs to the history.
    let (c, o, e) = run(
        &archive,
        &home,
        &["restore", "p", "--at", &iso(earliest), "--preview"],
    );
    assert_eq!(c, 0, "the start of the history is a usable moment: {o}{e}");

    // And when the two moments are a whole second apart, the plain form is kept: no reader needs
    // milliseconds to tell 08:34:46 from 08:34:48.
    let (c, _o, e) = run(
        &archive,
        &home,
        &["restore", "p", "--at", &iso(earliest - 1500), "--preview"],
    );
    assert_ne!(c, 0);
    let line = e.lines().next().unwrap_or("").to_string();
    let (asked, start) = two_moments(&line);
    assert!(
        !asked.contains('.') && !start.contains('.'),
        "a second apart keeps the plain form: {line}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// Pull the two moments out of "moment A is earlier than the start of the available history B.".
fn two_moments(line: &str) -> (String, String) {
    let w: Vec<&str> = line.split_whitespace().collect();
    let after = |anchor: &str| -> String {
        if let Some(i) = w.iter().position(|x| *x == anchor) {
            if i + 2 < w.len() {
                return format!("{} {}", w[i + 1], w[i + 2].trim_end_matches('.'));
            }
        }
        String::new()
    };
    (after("moment"), after("history"))
}

/// The window's own placeholder must never reach the core: an entry that asks for a value is refused
/// before it is run, and a value that begins with a dash is refused rather than read as a flag. This
/// is the server's rule (`app/src/menu.rs`), stated here as the core sees it: the core is never asked
/// to interpret a value as an option.
#[test]
fn the_core_is_never_handed_a_placeholder_or_a_flag_shaped_value() {
    let root = tmp("placeholder");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("a.txt"), "hello\n").unwrap();
    let archive = root.join("archive");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    run(&archive, &home, &["init-archive", archive.to_str().unwrap()]);
    let (c, _o, e) = run(
        &archive,
        &home,
        &["add", work.to_str().unwrap(), "--name", "p", "--preset", "auto", "--yes"],
    );
    assert_eq!(c, 0, "add: {e}");

    // A literal placeholder is not a path that exists in the archive: asking for its bytes must fail
    // rather than quietly answer about something else. (Note `why` *does* answer for an unknown path
    // — exit 0, "NOT tracked, reason not_in_preset" — which is correct: that is a real question about
    // a path, and the core answers it. `cat` is where the placeholder would have to be a real file.)
    let (c, _o, e) = run(&archive, &home, &["cat", "p", "--path", "{input}"]);
    assert_ne!(c, 0, "a placeholder must not be answered as if it were a stored file");
    assert!(e.contains("not in the state"), "and the refusal must say why: {e}");

    // A value that begins with a dash is read as an option by the core, and the real value is then
    // silently dropped: here the label the person asked for never reaches the archive — the mark is
    // saved with the default label while `--json` changed the *output shape* instead. That is why the
    // menu refuses such a value before it is ever handed over (`app/src/menu.rs::substitute`).
    let (c, o, _e) = run(&archive, &home, &["snap", "p", "--json"]);
    assert_eq!(c, 0, "the core accepts the flag shape — and does something else entirely");
    assert!(
        o.contains("\"label\""),
        "the dash-shaped value was read as a flag: the answer came back as JSON: {o}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// Days since the Unix epoch to (year, month, day), for building the ISO form without a date crate.
fn civil_from_ms(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = ((rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    ((if m <= 2 { y + 1 } else { y }), m, d, h, mi, s)
}
