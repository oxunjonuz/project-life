//! Round 302 — the author, his address and the sentence about what the program is for.
//!
//! The owner asked for them "everywhere it makes sense, like about the program", with the sentence
//! he wrote himself: the program is made for the moment an agent deletes or breaks something, and it
//! keeps everything.
//!
//! A string requested in a dozen files written in six languages is a string that drifts, so the
//! values live in exactly one place (`src/brand.rs`) and this file proves the promise from the
//! outside: it re-reads that source itself, runs the shipped binary, and requires the two to agree —
//! and requires that no *other* Rust file has quietly grown a second copy.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(core()).args(args).output().expect("run the core");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The values, read here directly from `src/brand.rs` — a second reader, so that a mistake in the
/// module (or in `tools/brand.py`) cannot hide behind the module's own answer.
fn from_source(name: &str) -> String {
    let text = fs::read_to_string("src/brand.rs").expect("src/brand.rs");
    let needle = format!("pub const {name}: &str = \"");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(&needle) {
            return rest.trim_end_matches("\";").to_string();
        }
    }
    panic!("src/brand.rs does not define {name} on one line");
}

#[test]
fn the_core_prints_the_name_and_the_address_read_from_the_source() {
    let (code, out, err) = run(&["version", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let j: serde_json::Value = serde_json::from_str(out.trim().lines().last().unwrap()).expect("JSON");
    assert_eq!(j["author"].as_str(), Some(from_source("AUTHOR").as_str()), "{j}");
    assert_eq!(j["authorEmail"].as_str(), Some(from_source("AUTHOR_EMAIL").as_str()), "{j}");
    assert_eq!(j["whatItIs"].as_str(), Some(from_source("WHAT_IT_IS").as_str()), "{j}");
    assert_eq!(j["product"].as_str(), Some(from_source("PRODUCT").as_str()), "{j}");
    assert_eq!(
        j["by"].as_str(),
        Some(format!("{} <{}>", from_source("AUTHOR"), from_source("AUTHOR_EMAIL")).as_str()),
        "{j}"
    );
}

/// The one command a person runs to ask "what is this?" has to answer it in words, not in a name.
#[test]
fn the_human_version_output_says_who_made_it_and_what_it_is_for() {
    let (code, out, _err) = run(&["version"]);
    assert_eq!(code, 0);
    assert!(out.contains(&from_source("PRODUCT")), "{out}");
    assert!(out.contains(&from_source("WHAT_IT_IS")), "{out}");
    assert!(out.contains(&from_source("AUTHOR")), "{out}");
    assert!(out.contains(&from_source("AUTHOR_EMAIL")), "{out}");
    assert!(out.contains(&from_source("COPYRIGHT")), "{out}");
    // The sentence the owner wrote, in his own words: an agent that deletes or breaks something, and
    // a program that keeps everything. Both halves must survive an edit of the constants.
    let what = from_source("WHAT_IT_IS").to_lowercase();
    assert!(what.contains("agent"), "{what}");
    assert!(what.contains("keeps everything"), "{what}");
}

#[test]
fn the_help_header_names_the_author_too() {
    for form in [["--help"], ["help"], ["-h"]] {
        let (code, out, _err) = run(&form);
        assert_eq!(code, 0, "{form:?} must not report failure: {out}");
        assert!(out.contains(&from_source("AUTHOR")), "{form:?}: {out}");
        assert!(out.contains(&from_source("AUTHOR_EMAIL")), "{form:?}: {out}");
    }
}

/// `pl --version` is two keystrokes away from `pl version`, and until round 302 the long form printed
/// the usage and exited 1 — the answer was there, the exit code told a script it had failed.
#[test]
fn the_long_form_of_version_answers_and_exits_zero() {
    let (code, out, err) = run(&["--version"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains(&from_source("AUTHOR")), "{out}");
    assert!(out.contains(&from_source("WHAT_IT_IS")), "{out}");
}

/// An option nobody knows is named, not swallowed: silence here would look like success.
#[test]
fn an_unrecognised_option_with_no_command_is_named() {
    let (code, _out, err) = run(&["--definitely-not-an-option"]);
    assert_eq!(code, 1);
    assert!(err.contains("--definitely-not-an-option"), "{err}");
}

/// The anti-drift rule, stated as a test: the name and the address are written in `src/brand.rs` and
/// nowhere else in the Rust source. A second copy compiles fine and is wrong six months later.
#[test]
fn no_other_rust_file_holds_a_second_copy_of_the_name_or_the_address() {
    let email = from_source("AUTHOR_EMAIL");
    let author = from_source("AUTHOR");
    let mut offenders = Vec::new();
    for dir in ["src", "tests", "app/src"] {
        walk(Path::new(dir), &mut |p: &Path| {
            if p.file_name().and_then(|n| n.to_str()) == Some("brand.rs") {
                return;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                return;
            }
            let Ok(text) = fs::read_to_string(p) else { return };
            if text.contains(&email) || text.contains(&author) {
                offenders.push(p.to_path_buf());
            }
        });
    }
    assert!(
        offenders.is_empty(),
        "these files hold a second copy of the author or the address: {offenders:?}. The values live \
         in src/brand.rs; everything else reads them (tools/brand.py, or `pl version --json`)."
    );
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

/// The brand module's own guard: the address has the shape of an address, and the sentence has both
/// halves of the owner's description. (`src/brand.rs` also tests this; this is the shipped-binary
/// side of the same promise, so a constant changed in the module cannot pass unnoticed.)
#[test]
fn the_address_has_the_shape_of_an_address() {
    let email = from_source("AUTHOR_EMAIL");
    assert_eq!(email.matches('@').count(), 1, "{email}");
    let (local, domain) = email.split_once('@').unwrap();
    assert!(!local.is_empty() && !domain.is_empty(), "{email}");
    assert!(domain.contains('.'), "{email}");
    assert!(!email.contains(char::is_whitespace), "{email}");
}
