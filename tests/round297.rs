//! Round 297 — the commands the window needs to speak to in JSON, tested from outside the program.
//!
//! The window is a shell around this core. Everything it shows has to come from a command that can
//! be read by a program, and every such command has to keep its human output unchanged for the
//! person at the terminal. These tests check the structured answers against the archive itself:
//! they never read the JSON as the only witness for the fact it reports.

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
    let p = std::env::temp_dir().join(format!("pl297-{}-{}", name, std::process::id()));
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

/// The last complete JSON document in a command's output, the same way the window reads it.
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

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = tmp(name);
        let work = root.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("src/a.txt"), "first\n").unwrap();
        fs::write(work.join("src/b.txt"), "second\n").unwrap();
        let f = Fixture {
            work,
            archive: root.join("archive"),
            home: root.join("home"),
            root,
        };
        fs::create_dir_all(&f.home).unwrap();
        let (c, _, e) = run(&f.archive, &f.home, &["init-archive", f.archive.to_str().unwrap()]);
        assert_eq!(c, 0, "init-archive: {e}");
        let (c, o, e) = run(
            &f.archive,
            &f.home,
            &["add", f.work.to_str().unwrap(), "--name", "p", "--preset", "auto", "--yes"],
        );
        assert_eq!(c, 0, "add: {o}{e}");
        f
    }

    fn pl(&self, args: &[&str]) -> (i32, String, String) {
        run(&self.archive, &self.home, args)
    }

    /// The project's folder inside the archive. The archive names it by id, not by name, so the
    /// test asks the filesystem rather than assuming a layout.
    fn project_dir(&self) -> PathBuf {
        let mut found: Vec<PathBuf> = fs::read_dir(self.archive.join("projects"))
            .expect("projects/ exists")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(found.len(), 1, "one project was added");
        found.pop().unwrap()
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.project_dir().join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(hash)
    }

    fn pl_json(&self, args: &[&str]) -> serde_json::Value {
        let mut a: Vec<&str> = args.to_vec();
        a.push("--json");
        let (c, o, e) = self.pl(&a);
        assert_eq!(c, 0, "{} -> exit {c}: {e}\n{o}", a.join(" "));
        json_of(&o)
    }
}

/// Every file under a tree with its hash — so "nothing was written" is a measurement, not a claim.
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
                let data = fs::read(&p).unwrap_or_default();
                out.push((rel, data.len() as u64, crate::digest(&data)));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

mod digest {
    //! A local SHA-256 so the test does not depend on the program's own hashing code.
    pub fn sha256(data: &[u8]) -> String {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut msg = data.to_vec();
        let bitlen = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bitlen.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h2) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = h2
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                h2 = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(h2);
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }
}

fn digest(data: &[u8]) -> String {
    digest::sha256(data)
}

// ---------------------------------------------------------------------------------------------

#[test]
fn why_json_agrees_with_blame_json_about_the_same_file() {
    let f = Fixture::new("why-vs-blame");
    fs::write(f.work.join("src/a.txt"), "second revision\n").unwrap();
    f.pl(&["scan-once", "p"]);

    let why = f.pl_json(&["why", "p", "src/a.txt"]);
    let blame = f.pl_json(&["blame", "p", "src/a.txt"]);
    let put_events = blame["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "put")
        .count();
    assert_eq!(
        why["versionCount"].as_u64().unwrap() as usize,
        put_events,
        "two independent commands must count the same versions"
    );
    assert!(why["tracked"].as_bool().unwrap());
    assert_eq!(why["decision"]["track"].as_bool(), Some(true));
    // the version list is chronological and its last entry is the last version
    let versions = why["versions"].as_array().unwrap();
    assert!(versions.len() >= 2, "a changed file has at least two versions");
    let last = &versions[versions.len() - 1];
    assert_eq!(last["hash"], why["lastVersion"]["hash"]);
    let data = fs::read(f.work.join("src/a.txt")).unwrap();
    assert_eq!(last["hash"].as_str().unwrap(), digest(&data), "the newest version is the file's own bytes");
}

#[test]
fn cat_json_returns_the_bytes_of_that_moment_and_marks_binary_as_binary() {
    let f = Fixture::new("cat-json");
    fs::write(f.work.join("src/a.txt"), "version one\n").unwrap();
    f.pl(&["scan-once", "p"]);
    let why = f.pl_json(&["why", "p", "src/a.txt"]);
    let first_at = why["versions"][0]["atIso"].as_str().unwrap().to_string();

    let cat = f.pl_json(&["cat", "p", "--path", "src/a.txt", "--at", &first_at]);
    // The first version of this path is the file as it stood when the project was added.
    assert_eq!(cat["text"].as_str().unwrap(), "first\n");
    assert_eq!(cat["encoding"], "utf8");
    assert_eq!(cat["truncated"], false);
    assert_eq!(cat["bytes"].as_u64().unwrap(), 6);
    assert_eq!(cat["sha256"].as_str().unwrap(), digest(b"first\n"), "the hash is the content's own");
    assert_eq!(cat["atIso"].as_str().unwrap(), first_at);

    // A file whose bytes are not text is reported as binary, with its size and hash — not printed
    // as if it were text. The extension says .txt, so this is the honest case: the filter decides
    // by name, and the window must still not pretend these bytes are readable.
    let raw: Vec<u8> = vec![0u8, 159, 146, 150, 255, 0, 12];
    fs::write(f.work.join("src/notes.txt"), &raw).unwrap();
    f.pl(&["scan-once", "p"]);
    let cat = f.pl_json(&["cat", "p", "--path", "src/notes.txt"]);
    assert_eq!(cat["encoding"], "binary");
    assert!(cat["text"].is_null(), "binary content is not offered as text");
    assert_eq!(cat["bytes"].as_u64().unwrap(), 7);
    assert_eq!(cat["sha256"].as_str().unwrap(), digest(&raw));
}

#[test]
fn cat_json_caps_what_it_hands_over_and_says_that_it_did() {
    let f = Fixture::new("cat-cap");
    let big = "x".repeat(300 * 1024);
    fs::write(f.work.join("src/big.txt"), &big).unwrap();
    f.pl(&["scan-once", "p"]);
    let cat = f.pl_json(&["cat", "p", "--path", "src/big.txt"]);
    assert!(cat["truncated"].as_bool().unwrap(), "a 300 KiB file must be reported as truncated");
    assert_eq!(cat["shownBytes"].as_u64().unwrap(), 256 * 1024);
    assert_eq!(cat["bytes"].as_u64().unwrap(), 300 * 1024);
    assert_eq!(cat["text"].as_str().unwrap().len(), 256 * 1024);
    // The hash is of the whole file, not of what was shown.
    assert_eq!(cat["sha256"].as_str().unwrap(), digest(big.as_bytes()));
}

#[test]
fn last_good_json_says_there_is_none_and_then_finds_one_after_a_mass_event() {
    let f = Fixture::new("last-good");
    let none = f.pl_json(&["last-good", "p"]);
    assert_eq!(none["found"], false);

    // A mass deletion: more than half the tracked files go at once.
    for i in 0..6 {
        fs::write(f.work.join(format!("src/f{i}.txt")), "x\n").unwrap();
    }
    f.pl(&["scan-once", "p"]);
    for i in 0..6 {
        fs::remove_file(f.work.join(format!("src/f{i}.txt"))).unwrap();
    }
    f.pl(&["scan-once", "p"]);
    let found = f.pl_json(&["last-good", "p"]);
    assert_eq!(found["found"], true, "a mass event must produce a last-good point: {found}");
    assert!(found["at"].as_i64().unwrap() > 0);
    assert!(found["why"].as_str().unwrap().contains("mass"));
}

#[test]
fn retention_json_reports_the_stored_policy_and_what_it_would_do() {
    let f = Fixture::new("retention-json");
    let empty = f.pl_json(&["retention", "p"]);
    assert_eq!(empty["stored"], false);
    assert!(empty["policy"].is_null());

    fs::write(f.work.join("src/a.txt"), "another revision\n").unwrap();
    f.pl(&["scan-once", "p"]);
    let (c, o, e) = f.pl(&["retention", "p", "7d:all,30d:1/day"]);
    assert_eq!(c, 0, "{o}{e}");
    let stored = f.pl_json(&["retention", "p"]);
    assert_eq!(stored["policy"].as_str().unwrap(), "7d:all,30d:1/day");
    let plan = &stored["plan"];
    assert!(plan["versionsBefore"].as_u64().unwrap() >= plan["versionsKept"].as_u64().unwrap());
    assert_eq!(plan["dropped"].as_u64().unwrap(), plan["versionsBefore"].as_u64().unwrap() - plan["versionsKept"].as_u64().unwrap());
    assert!(plan["newHistoryStartsAt"].as_i64().unwrap() > 0);
}

#[test]
fn audit_json_sees_a_changed_archive_file_and_an_unchanged_one() {
    let f = Fixture::new("audit-json");
    // Record a digest, then change one file inside the archive behind the program's back.
    let (c, o, e) = f.pl(&["audit-archive", "--update"]);
    assert_eq!(c, 0, "the digest must be recorded: {o}{e}");
    let (code, out, _) = f.pl(&["audit-archive", "--json"]);
    let clean = json_of(&out);
    assert_eq!(clean["ok"], true, "an unchanged archive must pass (exit {code}): {clean}");
    assert_eq!(clean["digestWritten"], false, "asking for the audit must not write a new digest");
    assert!(clean["filesHashed"].as_u64().unwrap() > 0);

    let victim = f.project_dir().join("project.json");
    let mut text = fs::read_to_string(&victim).unwrap();
    text = text.replace("\"p\"", "\"p-renamed\"");
    fs::write(&victim, text).unwrap();
    let (code, out, _) = f.pl(&["audit-archive", "--json"]);
    assert_eq!(code, 1, "a difference is an error exit, not a quiet success");
    let dirty = json_of(&out);
    assert_eq!(dirty["ok"], false, "a substituted file must be reported");
    assert!(!dirty["differences"].as_array().unwrap().is_empty());
}

#[test]
fn quarantine_json_lists_what_is_held() {
    let f = Fixture::new("quarantine-json");
    let empty = f.pl_json(&["quarantine"]);
    assert_eq!(empty["count"].as_u64().unwrap(), 0);

    // Put a blob in quarantine exactly as the program does when a stored blob fails its hash.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    let blobs = f.project_dir().join("blobs");
    let mut blob_files: Vec<PathBuf> = Vec::new();
    walk(&blobs, &mut blob_files);
    let q = f.project_dir().join("quarantine");
    fs::create_dir_all(&q).unwrap();
    let mut moved = 0;
    for p in blob_files.iter().take(1) {
        fs::rename(p, q.join(p.file_name().unwrap())).unwrap();
        moved += 1;
    }
    assert_eq!(moved, 1, "the fixture must have at least one blob");
    let listed = f.pl_json(&["quarantine"]);
    assert_eq!(listed["count"].as_u64().unwrap(), 1);
    assert_eq!(listed["entries"][0]["project"], "p");
    assert!(listed["entries"][0]["bytes"].as_u64().unwrap() > 0);
}

#[test]
fn recover_json_says_plainly_that_there_was_nothing_to_recover() {
    let f = Fixture::new("recover-json");
    let r = f.pl_json(&["recover", "p"]);
    assert_eq!(r["recovered"], false);
    assert!(r["action"].as_str().unwrap().contains("nothing to recover"));
    assert!(r["journalEvents"].as_u64().unwrap() > 0, "the journal is still readable afterwards");
    assert_eq!(r["trailingPartialLine"], false);
}

#[test]
fn apply_filters_json_counts_what_it_did_and_names_what_it_stopped_tracking() {
    let f = Fixture::new("apply-filters-json");
    fs::write(f.work.join("src/notes.log"), "log line\n").unwrap();
    let r = f.pl_json(&["apply-filters", "p"]);
    assert!(r["filesOnDisk"].as_u64().unwrap() >= 2);
    assert!(r["skippedByReason"].is_object());
    assert!(r["noLongerTracked"].is_array());
    assert!(r["note"].as_str().unwrap().contains("kept"), "the note must say earlier versions are kept");
}

#[test]
fn mark_and_snap_json_report_the_moment_and_the_event_really_exists() {
    let f = Fixture::new("mark-json");
    let m = f.pl_json(&["mark", "p", "before the refactor"]);
    assert_eq!(m["label"], "before the refactor");
    let at = m["at"].as_i64().unwrap();
    assert!(at > 0);
    let log = f.pl_json(&["log", "p"]);
    let marks: Vec<&serde_json::Value> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "mark" && e["label"] == "before the refactor")
        .collect();
    assert_eq!(marks.len(), 1, "the mark the JSON reported must be in the journal");

    let s = f.pl_json(&["snap", "p"]);
    assert!(s["label"].as_str().unwrap().starts_with("snap "), "a snapshot names itself by its moment");
    assert!(s["atIso"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn prune_json_reports_the_plan_before_it_deletes_and_the_outcome_after() {
    let f = Fixture::new("prune-json");
    fs::write(f.work.join("src/a.txt"), "rev 2\n").unwrap();
    f.pl(&["scan-once", "p"]);
    fs::write(f.work.join("src/a.txt"), "rev 3\n").unwrap();
    f.pl(&["scan-once", "p"]);

    let dry = f.pl_json(&["prune", "p", "--policy", "7d:all,30d:1/day", "--dry-run"]);
    assert_eq!(dry["dryRun"], true);
    assert_eq!(dry["applied"], false);
    let before = dry["versionsBefore"].as_u64().unwrap();
    assert!(before >= 1);

    let (code, out, _) = f.pl(&["prune", "p", "--policy", "7d:all,30d:1/day", "--json", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let applied = json_of(&out);
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["dryRun"], false);
    assert!(applied["versionsAfter"].as_u64().unwrap() <= before);
}

#[test]
fn note_config_and_rebuild_cache_also_answer_in_json() {
    let f = Fixture::new("misc-json");
    let n = f.pl_json(&["note", "p", "a note for the owner"]);
    assert_eq!(n["note"], "a note for the owner");
    assert_eq!(n["project"], "p");

    let cfg = f.pl_json(&["config", "get"]);
    assert!(cfg["intervalSeconds"].is_number(), "every setting comes back as a document: {cfg}");

    let one = f.pl_json(&["config", "get", "intervalSeconds"]);
    assert_eq!(one["key"], "intervalSeconds");

    let rc = f.pl_json(&["rebuild-cache", "p"]);
    assert_eq!(rc["rebuilt"][0]["project"], "p");
    assert!(rc["rebuilt"][0]["files"].as_u64().unwrap() > 0);
}

#[test]
fn content_search_answers_in_json_and_stays_honest_about_what_it_cannot_search() {
    let f = Fixture::new("content-json");
    fs::write(f.work.join("src/a.txt"), "needle in a haystack\n").unwrap();
    f.pl(&["scan-once", "p"]);
    let hits = f.pl_json(&["log", "p", "--content", "needle"]);
    assert!(hits["matches"].as_array().unwrap().len() >= 1);
    assert_eq!(hits["needle"], "needle");
    assert!(hits["tooBigToSearch"].as_u64().is_some(), "the count of skipped-too-big versions must be reported");
    let hash = hits["matches"][0]["hash"].as_str().unwrap();
    let stored = fs::read(f.blob_path(hash)).unwrap();
    assert!(String::from_utf8_lossy(&stored).contains("needle"), "the match names a blob that really holds it");
}

#[test]
fn restore_preview_json_writes_nothing_and_lists_what_it_would_write() {
    let f = Fixture::new("preview-json");
    let target = f.root.join("preview-target");
    fs::write(f.work.join("src/a.txt"), "changed after the moment\n").unwrap();
    f.pl(&["scan-once", "p"]);
    let log = f.pl_json(&["log", "p"]);
    let first_put = log.as_array().unwrap().iter().find(|e| e["type"] == "put").unwrap();
    let at = first_put["ts"].as_i64().unwrap();
    let iso = {
        // the journal's own ISO form for that moment, taken from `why --json`
        let w = f.pl_json(&["why", "p", "src/a.txt"]);
        w["versions"][0]["atIso"].as_str().unwrap().to_string()
    };
    assert!(at > 0);

    let before = snapshot(&f.archive);
    let plan = f.pl_json(&["restore", "p", "--at", &iso, "--to", target.to_str().unwrap(), "--preview"]);
    assert!(plan["create"].as_u64().unwrap() >= 1);
    assert_eq!(plan["total"].as_u64().unwrap(), plan["create"].as_u64().unwrap() + plan["overwrite"].as_u64().unwrap());
    assert!(plan["files"].as_array().unwrap().iter().any(|f| f["path"] == "src/a.txt"));
    assert_eq!(snapshot(&f.archive), before, "a preview must not touch the archive");
    assert!(!target.exists(), "a preview must not create the target folder");
}

/// The same promise as above, for the route the *person* uses: `--preview` on a terminal, without
/// `--json`. Found by the mutation campaign: removing the preview's early return left every test
/// green, because the only preview test asked for JSON, and the JSON preview returns before that
/// code is reached. A preview that writes is the one thing preview must never do.
#[test]
fn preview_without_json_writes_nothing_either() {
    let f = Fixture::new("preview-plain");
    let target = f.root.join("preview-plain-target");
    fs::write(f.work.join("src/a.txt"), "changed after the moment\n").unwrap();
    f.pl(&["scan-once", "p"]);
    let why = f.pl_json(&["why", "p", "src/a.txt"]);
    let first = why["versions"][0]["atIso"].as_str().unwrap().to_string();

    let before = snapshot(&f.archive);
    let (code, out, err) = f.pl(&["restore", "p", "--at", &first, "--to", target.to_str().unwrap(), "--preview"]);
    assert_eq!(code, 0, "preview exited {code}: {out}{err}");
    assert!(out.contains("Files:") || out.contains("Target:"), "the plan was printed: {out}");
    assert!(!target.exists(), "a preview must not create the target folder");
    assert_eq!(snapshot(&f.archive), before, "a preview must not touch the archive");

    // and the same call without --preview really does write, so the test is not passing by accident
    let (code, out, err) = f.pl(&["restore", "p", "--at", &first, "--to", target.to_str().unwrap(), "--yes"]);
    assert_eq!(code, 0, "restore exited {code}: {out}{err}");
    assert!(target.join("src/a.txt").is_file(), "a real restore writes the file");
}
