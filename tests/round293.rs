//! Round 293 acceptance tests: the **partial pass** — a notification-driven cycle that walks only
//! the paths the notifications named.
//!
//! Every test runs on a real filesystem and reads its result back from disk: the journal, the state
//! cache, the blobs, and (for the daemon-driven ones) the trigger state the daemon publishes in
//! `watch_state.json`. The helpers are duplicated from `acceptance.rs`/`round292.rs` on purpose:
//! integration tests are separate crates, and those files already prove the earlier rounds.
//!
//! The one thing every test here has to prove *in addition* to its own requirement is that the pass
//! really was partial: `partialCycles` says a notification-driven pass ran, and `lastPartial` says
//! how much of the project it looked at. A partial pass that quietly became a full pass would
//! satisfy the functional half of most of these tests, and that is exactly what the numbers are for.

use projectlife::archive::{iso_ms, Archive, Project};
use projectlife::events;
use projectlife::scan::{self, ScanOptions};
use projectlife::store::{self, Store};
use projectlife::util;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    base: PathBuf,
    pub arch: Archive,
    pub project: Project,
    pub project_root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn write(path: &Path, data: &[u8]) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, data).unwrap();
}

fn setup(name: &str, files: &[(&str, &[u8])]) -> Fixture {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("projectlife-293-{}-{}-{}", name, std::process::id(), id));
    let _ = fs::remove_dir_all(&base);
    let project_root = base.join("project");
    fs::create_dir_all(&project_root).unwrap();
    for (rel, data) in files {
        write(&project_root.join(rel), data);
    }
    let archive_root = base.join("archive");
    let mut arch = Archive::create(&archive_root).unwrap();
    arch.config.set("stopFreePercent", Value::from(0));
    arch.config.set("warnFreePercent", Value::from(0));
    arch.config.save(&archive_root).unwrap();
    let dir = arch.projects_dir().join("00000000-0000-4000-8000-000000000001");
    fs::create_dir_all(&dir).unwrap();
    let meta = json!({
        "schemaVersion": 1,
        "projectId": "00000000-0000-4000-8000-000000000001",
        "name": name,
        "projectRoot": project_root.to_string_lossy(),
        "createdAt": iso_ms(util::now_ms()),
        "historyStartsAt": iso_ms(util::now_ms()),
        "profile": "source",
        "settings": {},
        "state": "active",
        "lastSeq": 0
    });
    fs::write(dir.join("project.json"), serde_json::to_string_pretty(&meta).unwrap()).unwrap();
    let project = Project::from_dir(&dir).unwrap();
    Fixture { base, arch, project, project_root }
}

fn reload(f: &Fixture) -> Project {
    Project::from_dir(&f.project.dir).unwrap()
}

fn scan_initial(f: &mut Fixture) {
    let opts = ScanOptions { reason: "initial".into(), deep: true, with_initial_snapshot: true, ..Default::default() };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap();
}

fn scan_now(f: &mut Fixture) -> scan::ScanReport {
    let opts = ScanOptions { reason: "observed".into(), deep: false, ..Default::default() };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap()
}

/// The partial pass, driven directly: the same function the daemon calls when a notification fires,
/// with the path list a notification would have produced.
fn partial(f: &mut Fixture, paths: &[&str]) -> scan::ScanReport {
    let ps: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
    let p = reload(f);
    f.project = p;
    scan::scan_project_partial(&f.arch, &mut f.project, &ps, "changed").unwrap()
}

fn journal(f: &Fixture) -> Vec<events::Ev> {
    events::load_journal(&f.project.dir).unwrap_or(events::Journal { events: Vec::new(), trailing_partial: false }).events
}

/// Every event after the initial snapshot as `(type, path, from, to)`.
fn tail(f: &Fixture) -> Vec<(String, String, String, String)> {
    journal(f)
        .iter()
        .skip_while(|e| events::event_type(e) == "snapshot")
        .map(|e| {
            (
                events::event_type(e).to_string(),
                events::get_str(e, "path").unwrap_or_default(),
                events::get_str(e, "from").unwrap_or_default(),
                events::get_str(e, "to").unwrap_or_default(),
            )
        })
        .collect()
}

fn kinds(f: &Fixture, kind: &str) -> Vec<(String, String, String, String)> {
    tail(f).into_iter().filter(|(t, _, _, _)| t == kind).collect()
}

fn puts(f: &Fixture) -> Vec<(String, String)> {
    journal(f)
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .map(|e| (events::get_str(e, "path").unwrap_or_default(), events::get_str(e, "hash").unwrap_or_default()))
        .collect()
}

fn cache_of(f: &Fixture) -> scan::StateCache {
    scan::StateCache::load(&f.project, &journal(f))
}

fn blob_count(f: &Fixture) -> usize {
    Store::new(&f.project.blobs_dir(), &f.project.tmp_dir()).list_all().len()
}

fn hash_of(path: &Path) -> String {
    store::sha256_bytes(&fs::read(path).unwrap_or_default())
}

/// The archive's own invariant: every blob the journal's state references exists, and no blob is
/// left unreferenced. A partial pass that wrote an event and lost the bytes (or a blob nobody points
/// at) fails here.
fn consistent(f: &Fixture) -> Result<(), String> {
    let j = journal(f);
    let st = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for e in &j {
        if events::event_type(e) == "put" {
            if let Some(h) = events::get_str(e, "hash") {
                referenced.insert(h);
            }
        }
    }
    let missing: Vec<String> = referenced.iter().filter(|h| !st.has(h)).cloned().collect();
    if !missing.is_empty() {
        return Err(format!("referenced blobs absent: {missing:?}"));
    }
    let dangling: Vec<String> =
        st.list_all().into_iter().map(|(h, _)| h).filter(|h| !referenced.contains(h)).collect();
    if !dangling.is_empty() {
        return Err(format!("unreferenced blobs left behind: {dangling:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The daemon harness (round 290/292 shape): a real process, a real filesystem, real inotify events.
// ---------------------------------------------------------------------------------------------

fn run_bin(home: &Path, args: &[&str]) -> std::process::Output {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", home);
    c.args(args);
    c.output().expect("the shipped binary must be runnable")
}

fn spawn_daemon(f: &Fixture, envs: &[(&str, &str)]) -> std::process::Child {
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", &home);
    c.arg("--archive").arg(f.arch.root.to_string_lossy().to_string()).arg("daemon").arg("run");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c.spawn().expect("the shipped binary must be runnable")
}

fn stop_daemon(child: &mut std::process::Child) {
    let _ = std::process::Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
    for _ in 0..100 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A running daemon that is killed when the test ends, however it ends.
///
/// Without this, an assertion that fails half-way through a test leaks a daemon process that lives
/// for ever: round 293 found 46 such processes left by earlier campaigns, which is how the leak was
/// noticed — the test harness had been relying on every test reaching its own `stop_daemon`.
struct Daemon(std::process::Child);

impl Daemon {
    fn start(f: &Fixture, envs: &[(&str, &str)]) -> Daemon {
        Daemon(spawn_daemon(f, envs))
    }
    fn id(&self) -> u32 {
        self.0.id()
    }
    fn stop(&mut self) {
        stop_daemon(&mut self.0);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            stop_daemon(&mut self.0);
        }
    }
}

fn watch_state(f: &Fixture) -> Value {
    match fs::read_to_string(f.arch.root.join("watch_state.json")) {
        Ok(t) => serde_json::from_str(&t).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

fn st_u64(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn set_config(f: &Fixture, key: &str, v: Value) {
    let mut cfg = Archive::open(&f.arch.root).unwrap().config;
    cfg.set(key, v);
    cfg.save(&f.arch.root).unwrap();
}

fn set_interval(f: &Fixture, secs: i64) {
    set_config(f, "intervalSeconds", Value::from(secs));
    set_config(f, "autoInterval", Value::from(false));
}

/// Wait until this daemon has a watch set AND has completed its first (startup) pass: the state file
/// is written at the end of a pass, so `periodicCycles >= 1` proves the startup pass is over and a
/// change made now cannot be explained by it.
fn wait_for_watches(f: &Fixture, pid: u32, timeout_ms: i64) -> Value {
    let started = util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && st_u64(&st, "watchedDirs") > 0 && st_u64(&st, "periodicCycles") >= 1 {
            return st;
        }
        if util::now_ms() - started > timeout_ms {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn wait_for_state(f: &Fixture, pid: u32, timeout_ms: i64, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let started = util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && pred(&st) {
            return st;
        }
        if util::now_ms() - started > timeout_ms {
            eprintln!("wait_for_state: {what} not reached within {timeout_ms} ms; last state: {st}");
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn wait_for_current_bytes(f: &Fixture, rel: &str, timeout_ms: i64) -> Option<i64> {
    let want = hash_of(&f.project_root.join(rel));
    let started = util::now_ms();
    loop {
        if puts(f).iter().any(|(p, h)| p == rel && *h == want) {
            return Some(util::now_ms() - started);
        }
        if util::now_ms() - started > timeout_ms {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The `lastPartial` block of the published trigger state, for the project with this name.
fn last_partial(st: &Value, name: &str) -> Option<Value> {
    st.get("lastPartial")?
        .get("projects")?
        .as_array()?
        .iter()
        .find(|p| p.get("project").and_then(|v| v.as_str()) == Some(name))
        .cloned()
}

/// A project with `n` files in `src/`, so "walked one file instead of n" is a measurable statement.
fn many_files(name: &str, n: usize) -> Fixture {
    let f = setup(name, &[]);
    for i in 0..n {
        write(&f.project_root.join("src").join(format!("seed{i:04}.txt")), format!("seed {i}\n").as_bytes());
    }
    f
}

// =============================================================================================
// 1. Creation — a file created in a watched folder is stored by the notification-driven pass,
//    and the periodic pass did not run to do it.
// =============================================================================================
#[test]
fn partial_pass_stores_a_created_file_without_a_periodic_pass() {
    let mut f = many_files("p-create", 200);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "the watch set must be installed: {st}");

    write(&f.project_root.join("src/new.txt"), b"created\n");
    let latency = wait_for_current_bytes(&f, "src/new.txt", 12_000);
    let st = wait_for_state(&f, d.id(), 8_000, "partialCycles >= 1", |s| st_u64(s, "partialCycles") >= 1);
    d.stop();

    let latency = latency.unwrap_or_else(|| panic!("with a 60 s interval only a notification can explain this version: {st}"));
    assert!(latency < 12_000, "the notification must store the file long before the interval, took {latency} ms");
    assert_eq!(st_u64(&st, "periodicCycles"), 1, "only the startup pass may be periodic here: {st}");
    assert!(st_u64(&st, "triggerCycles") >= 1, "the pass must be recorded as notification-driven: {st}");
    assert_eq!(
        st_u64(&st, "partialCycles"),
        st_u64(&st, "triggerCycles"),
        "every notification-driven pass here must be partial: {st}"
    );

    // …and it really walked a scope, not the project: 201 tracked files, one in scope.
    let lp = last_partial(&st, "p-create").unwrap_or_else(|| panic!("the state must publish what the partial pass did: {st}"));
    let tracked = cache_of(&f).files.len();
    assert!(tracked >= 201, "the project itself is much larger than the scope ({tracked} tracked): {lp}");
    assert!(lp["scopePaths"].as_u64().unwrap_or(0) >= 1, "{lp}");
    // On a filesystem that reports its own reads as changes — this machine's host share is one; the
    // container's overlay is not; tools/inotify_probe.py measures it — the scope of a
    // notification-driven pass is legitimately wider than the path that was written by hand. The
    // strict form is therefore asserted only when the only notification that arrived is ours, and
    // the exact scope semantics are proved by the tests that hand the paths in themselves.
    if st_u64(&st, "events") == 1 {
        assert_eq!(lp["filesInScope"].as_u64(), Some(1), "exactly the notified file: {lp}");
        assert_eq!(lp["dirsWalked"].as_u64(), Some(0), "a notification about a file opens no directory: {lp}");
    } else {
        assert!(lp["filesInScope"].as_u64().unwrap_or(0) <= tracked as u64, "the scope cannot exceed the project: {lp}");
        assert!(lp["filesInScope"].as_u64().unwrap_or(0) >= 1, "{lp}");
        eprintln!("note: this filesystem reported {} notification paths; the scope was {lp}", st_u64(&st, "events"));
    }
    consistent(&f).unwrap();
}

// =============================================================================================
// 2. Modification — the new bytes are stored, with the hash of what is on disk.
// =============================================================================================
#[test]
fn partial_pass_stores_the_new_bytes_of_a_modified_file() {
    let mut f = many_files("p-mod", 200);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    let body = b"changed bytes, second version\n".to_vec();
    write(&f.project_root.join("src/seed0007.txt"), &body);
    let want = store::sha256_bytes(&body);
    let found = wait_for_current_bytes(&f, "src/seed0007.txt", 12_000);
    let st = wait_for_state(&f, d.id(), 8_000, "partialCycles >= 1", |s| st_u64(s, "partialCycles") >= 1);
    d.stop();

    assert!(found.is_some(), "the modified file must be stored by the notification: {st}");
    let versions: Vec<String> = puts(&f).into_iter().filter(|(p, _)| p == "src/seed0007.txt").map(|(_, h)| h).collect();
    assert_eq!(versions.last().map(|s| s.as_str()), Some(want.as_str()), "the stored hash is the hash on disk");
    assert_eq!(versions.len(), 2, "the initial version plus exactly one new one: {versions:?}");
    assert_eq!(st_u64(&st, "periodicCycles"), 1, "no periodic pass may explain this: {st}");
    consistent(&f).unwrap();
}

// =============================================================================================
// 3. Deletion — the notification names the PARENT directory; the pass must list it and find the
//    file that is gone.
// =============================================================================================
#[test]
fn partial_pass_finds_a_deleted_file_from_a_notification_about_its_parent() {
    let mut f = setup("p-del", &[("src/a.txt", b"a\n"), ("src/b.txt", b"b\n"), ("src/c.txt", b"c\n")]);
    scan_initial(&mut f);
    fs::remove_file(f.project_root.join("src/b.txt")).unwrap();

    let rep = partial(&mut f, &["src"]);
    assert!(rep.partial);
    assert_eq!(rep.deleted, 1, "the vanished file must be found by comparing the directory with the state");

    let dels = kinds(&f, "delete");
    assert_eq!(dels.len(), 1, "exactly one delete: {dels:?}");
    assert_eq!(dels[0].1, "src/b.txt");
    assert!(!cache_of(&f).files.contains_key("src/b.txt"), "the state must no longer claim the file exists");
    assert!(cache_of(&f).files.contains_key("src/a.txt"), "the untouched files stay tracked");
    assert_eq!(puts(&f).len(), 3, "an unchanged file must not produce a version: {:?}", puts(&f));
    assert!(kinds(&f, "move").is_empty(), "nothing was renamed: {:?}", kinds(&f, "move"));
    consistent(&f).unwrap();
}

// =============================================================================================
// 4. Rename inside one directory — the notification names both paths, the pass writes `move` and
//    stores no new bytes.
// =============================================================================================
#[test]
fn partial_pass_writes_one_move_for_a_rename_inside_a_directory() {
    let mut f = setup("p-mv", &[("src/a.ts", b"export const a = 1;\n"), ("src/other.ts", b"x\n")]);
    scan_initial(&mut f);
    let blobs_before = blob_count(&f);
    let hash_before = cache_of(&f).files.get("src/a.ts").map(|e| e.hash.clone()).unwrap();
    fs::rename(f.project_root.join("src/a.ts"), f.project_root.join("src/b.ts")).unwrap();

    let rep = partial(&mut f, &["src/a.ts", "src/b.ts"]);
    assert_eq!(rep.moved, 1, "{rep:?}");
    assert_eq!(rep.deleted, 0, "a rename must not be reported as a deletion");
    assert_eq!(rep.created, 0, "a rename must not be reported as a new file");
    assert_eq!(rep.blobs_new, 0, "the contents are known: no new blob");

    let mv = kinds(&f, "move");
    assert_eq!(mv.len(), 1, "{mv:?}");
    assert_eq!((mv[0].2.as_str(), mv[0].3.as_str()), ("src/a.ts", "src/b.ts"));
    assert!(kinds(&f, "delete").is_empty(), "no delete: {:?}", kinds(&f, "delete"));
    let c = cache_of(&f);
    assert!(c.files.contains_key("src/b.ts") && !c.files.contains_key("src/a.ts"));
    assert_eq!(
        c.files.get("src/b.ts").map(|e| e.hash.clone()),
        Some(hash_before),
        "the destination inherits the source's last known version"
    );
    assert_eq!(blob_count(&f), blobs_before, "no blob was added");
    consistent(&f).unwrap();
}

// =============================================================================================
// 5. Rename BETWEEN directories (critical) — notifications arrive for both directories, and the
//    pass must match the two sides. It must also be honest about the case where only one side
//    arrives: that is the documented limitation, and a full pass is what repairs it.
// =============================================================================================
#[test]
fn partial_pass_matches_a_rename_between_two_directories() {
    let mut f = setup("p-mv2", &[("src/a.ts", b"export const a = 1;\n"), ("lib/keep.txt", b"k\n")]);
    scan_initial(&mut f);
    let blobs_before = blob_count(&f);
    fs::rename(f.project_root.join("src/a.ts"), f.project_root.join("lib/b.ts")).unwrap();

    let rep = partial(&mut f, &["src/a.ts", "lib/b.ts"]);
    assert_eq!(rep.moved, 1, "the two sides must be matched: {rep:?}");
    assert_eq!(rep.deleted, 0);
    assert_eq!(rep.blobs_new, 0, "a rename does not create bytes");
    let mv = kinds(&f, "move");
    assert_eq!(mv.len(), 1, "{mv:?}");
    assert_eq!((mv[0].2.as_str(), mv[0].3.as_str()), ("src/a.ts", "lib/b.ts"));
    assert_eq!(blob_count(&f), blobs_before);
    consistent(&f).unwrap();

    // The lost-half case, on a fresh fixture: only the disappearance was notified. The pass then
    // writes a delete (it cannot invent a destination it was never told about) and the next FULL
    // pass stores the file at its new path. This is the limitation, written down and measured.
    let mut g = setup("p-mv2-lost", &[("src/a.ts", b"export const a = 1;\n"), ("lib/keep.txt", b"k\n")]);
    scan_initial(&mut g);
    fs::rename(g.project_root.join("src/a.ts"), g.project_root.join("lib/b.ts")).unwrap();
    partial(&mut g, &["src/a.ts"]);
    assert!(
        puts(&g).iter().all(|(p, _)| p != "lib/b.ts"),
        "a destination that was never mentioned must not be stored: {:?}",
        puts(&g)
    );
    let dels = kinds(&g, "delete");
    assert_eq!(dels.len(), 1, "the disappearance is a delete for this pass: {dels:?}");
    assert_eq!(dels[0].1, "src/a.ts");
    scan_now(&mut g);
    assert!(puts(&g).iter().any(|(p, _)| p == "lib/b.ts"), "the periodic full pass is what repairs it: {:?}", puts(&g));
    let st = events::state_at(&journal(&g), i64::MAX, None);
    assert!(st.contains_key("lib/b.ts") && !st.contains_key("src/a.ts"), "the current state is right again");
    consistent(&g).unwrap();
}

// =============================================================================================
// 6. Folder rename — one move per file, one batch.
// =============================================================================================
#[test]
fn partial_pass_writes_a_batch_of_moves_for_a_folder_rename() {
    let mut f = setup(
        "p-mvdir",
        &[("src/a.ts", b"a\n"), ("src/b.ts", b"b\n"), ("src/deep/c.ts", b"c\n"), ("keep.txt", b"k\n")],
    );
    scan_initial(&mut f);
    let blobs_before = blob_count(&f);
    fs::rename(f.project_root.join("src"), f.project_root.join("lib")).unwrap();

    let rep = partial(&mut f, &["src", "lib"]);
    assert_eq!(rep.moved, 3, "every file of the folder must be moved: {rep:?}");
    assert_eq!(rep.deleted, 0, "the files were renamed, not lost");
    assert_eq!(rep.blobs_new, 0);
    let mv = kinds(&f, "move");
    assert_eq!(mv.len(), 3, "{mv:?}");
    let moves: BTreeMap<String, String> = mv.iter().map(|(_, _, from, to)| (from.clone(), to.clone())).collect();
    assert_eq!(moves.get("src/a.ts").map(|s| s.as_str()), Some("lib/a.ts"));
    assert_eq!(moves.get("src/b.ts").map(|s| s.as_str()), Some("lib/b.ts"));
    assert_eq!(moves.get("src/deep/c.ts").map(|s| s.as_str()), Some("lib/deep/c.ts"));
    let batches: BTreeSet<String> = journal(&f)
        .iter()
        .filter(|e| events::event_type(e) == "move")
        .map(|e| events::get_str(e, "batchId").unwrap_or_default())
        .collect();
    assert_eq!(batches.len(), 1, "one folder rename is one batch: {batches:?}");
    assert_eq!(blob_count(&f), blobs_before);
    consistent(&f).unwrap();
}

// =============================================================================================
// 7. Mass deletion through a partial pass — 60 files vanish, and the mass event is written with
//    the sequence of the last good state, exactly as a full pass would write it.
// =============================================================================================
#[test]
fn partial_pass_writes_a_mass_delete_for_fifty_plus_files() {
    let mut f = many_files("p-mass", 60);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    for i in 0..60 {
        fs::remove_file(f.project_root.join(format!("src/seed{i:04}.txt"))).unwrap();
    }
    // The predicate reads the state file, not the journal: the journal is written *inside* the pass
    // and the state at the end of it, so a predicate on the journal alone can be satisfied by a pass
    // that is still running and then hand the assertions a state from before it (round 293 found
    // exactly this race under load). Waiting for the published counters means waiting for the pass.
    let st = wait_for_state(&f, d.id(), 25_000, "a partial pass with all 60 deletes", |s| {
        st_u64(s, "partialCycles") >= 1 && kinds(&f, "delete").len() >= 60
    });
    d.stop();

    let dels = kinds(&f, "delete");
    assert_eq!(dels.len(), 60, "every vanished file must be recorded: {} ({st})", dels.len());
    let mass: Vec<events::Ev> = journal(&f).into_iter().filter(|e| events::event_type(e) == "mass").collect();
    assert!(!mass.is_empty(), "a deletion of 60 tracked files is a mass event, and must be recorded as one");
    assert!(
        mass.iter().all(|m| events::get_str(m, "kind").as_deref() == Some("mass_delete")),
        "every mass event here is a mass deletion: {mass:?}"
    );
    assert_eq!(
        mass.iter().filter_map(|m| events::get_u64(m, "files")).sum::<u64>(),
        60,
        "every deleted file is accounted for by a mass event (a machine slow enough to split the \
         deletion into two passes produces two events, and both are counted here): {mass:?}"
    );
    let m = mass.iter().max_by_key(|m| events::get_u64(m, "files")).unwrap();
    assert!(events::get_u64(m, "files").unwrap_or(0) >= 50, "the event must cover the 50+ files: {m:?}");
    assert_eq!(events::get_u64(m, "deleted"), events::get_u64(m, "files"), "{m:?}");
    let last_good = events::get_u64(m, "lastGoodSeq").expect("the mass event carries the last good sequence");
    let j = journal(&f);
    let before: Vec<&events::Ev> = j.iter().filter(|e| events::seq_of(e) <= last_good).collect();
    assert!(!before.is_empty(), "the last good sequence must point at real events");
    assert!(
        before.iter().all(|e| !matches!(events::event_type(e), "delete" | "mass")),
        "the last good state must precede the deletions: {before:?}"
    );
    assert!(st_u64(&st, "partialCycles") >= 1, "the pass must be partial: {st}");
    assert_eq!(st_u64(&st, "periodicCycles"), 1, "the 60 s interval must not have been the trigger: {st}");
    consistent(&f).unwrap();
}

// =============================================================================================
// 8. A lost notification must not stop the periodic pass from storing the change, and — the other
//    half — a partial pass must not postpone the periodic deadline.
// =============================================================================================
#[test]
fn lost_notification_and_partial_pass_do_not_postpone_the_periodic_pass() {
    // (a) every notification is thrown away: the periodic pass is the only thing left that can store
    let mut f = setup("p-lost", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 2);
    let mut d = Daemon::start(&f, &[("PROJECTLIFE_DROP_EVENTS", "1")]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "the watch set is installed even though the events are dropped: {st}");
    write(&f.project_root.join("src/a.txt"), b"two\n");
    let latency = wait_for_current_bytes(&f, "src/a.txt", 15_000);
    let st = wait_for_state(&f, d.id(), 15_000, "lostEvents >= 1", |s| st_u64(s, "lostEvents") >= 1);
    d.stop();
    assert!(latency.is_some(), "a change whose notification was lost must still be stored by the periodic pass");
    assert!(st_u64(&st, "lostEvents") > 0, "the injector must really have dropped notifications: {st}");
    assert_eq!(st_u64(&st, "triggerCycles"), 0, "no pass may claim to be notification-driven: {st}");
    assert_eq!(st_u64(&st, "partialCycles"), 0, "…and none may claim to be partial: {st}");
    assert!(st_u64(&st, "periodicCycles") >= 1, "the periodic pass is what stored the change: {st}");
    consistent(&f).unwrap();

    // (b) notifications work, a partial pass runs — and the next periodic pass still comes within a
    //     bounded time after it (the deadline is measured from the end of the last pass of any kind,
    //     so a notification can only ever bring it forward).
    let mut g = setup("p-postpone", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut g);
    set_interval(&g, 3);
    let mut d = Daemon::start(&g, &[]);
    let st = wait_for_watches(&g, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");
    write(&g.project_root.join("src/a.txt"), b"two\n");
    let st = wait_for_state(&g, d.id(), 12_000, "partialCycles >= 1", |s| st_u64(s, "partialCycles") >= 1);
    assert!(st_u64(&st, "partialCycles") >= 1, "the change must have started a partial pass: {st}");
    let periodic_before = st_u64(&st, "periodicCycles");
    let seen_at = util::now_ms();
    let st2 = wait_for_state(&g, d.id(), 12_000, "the next periodic pass", |s| st_u64(s, "periodicCycles") > periodic_before);
    let waited = util::now_ms() - seen_at;
    d.stop();
    assert!(
        st_u64(&st2, "periodicCycles") > periodic_before,
        "the periodic pass must not be postponed by a notification-driven pass: {st2}"
    );
    assert!(
        waited <= 5_000,
        "the next periodic pass came {waited} ms after the partial pass with a 3 s interval: a partial pass is an optimisation, not a replacement ({st2})"
    );
}

// =============================================================================================
// 9. No duplicated versions: a partial pass and a following full pass must not store the same
//    contents twice — which also proves the state cache still describes the files nobody mentioned.
// =============================================================================================
#[test]
fn a_partial_pass_and_a_full_pass_do_not_duplicate_a_version() {
    let mut f = setup("p-dup", &[("src/a.txt", b"one\n"), ("src/b.txt", b"b\n"), ("src/c.txt", b"c\n")]);
    scan_initial(&mut f);
    set_interval(&f, 5);
    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    write(&f.project_root.join("src/a.txt"), b"two\n");
    assert!(wait_for_current_bytes(&f, "src/a.txt", 12_000).is_some(), "the notification-driven pass stores the change first");
    let st = wait_for_state(&f, d.id(), 12_000, "both kinds of pass", |s| {
        st_u64(s, "partialCycles") >= 1 && st_u64(s, "periodicCycles") >= 2
    });
    d.stop();
    assert!(st_u64(&st, "partialCycles") >= 1 && st_u64(&st, "periodicCycles") >= 2, "both kinds of pass really ran: {st}");

    // One more full pass over an unchanged tree must write nothing at all.
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let out = run_bin(&home, &["--archive", &archive, "scan-once", "p-dup", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let p0 = &v["projects"][0];
    assert_eq!(p0["created"].as_u64(), Some(0), "the cache must still know the untouched files: {p0}");
    assert_eq!(p0["changed"].as_u64(), Some(0), "{p0}");
    assert_eq!(p0["deleted"].as_u64(), Some(0), "{p0}");

    let all = puts(&f);
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for k in &all {
        *seen.entry(k.clone()).or_insert(0) += 1;
    }
    let dup: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
    assert!(dup.is_empty(), "no (path, hash) may be stored twice: {dup:?}");
    assert_eq!(all.iter().filter(|(p, _)| p == "src/a.txt").count(), 2, "one version per distinct content: {all:?}");
    consistent(&f).unwrap();
}

// =============================================================================================
// 10. A directory that arrives by rename: the notification names the directory, so the pass must
//     walk it (its files were never mentioned), and the new directory must join the watch set —
//     otherwise the NEXT change inside it would only be seen by the periodic pass.
// =============================================================================================
#[test]
fn a_directory_moved_into_the_project_is_walked_and_then_watched() {
    let mut f = setup("p-newdir", &[("keep.txt", b"k\n")]);
    scan_initial(&mut f);
    set_interval(&f, 60);

    // built outside the observed tree, so the only notification is about the directory itself
    let staging = f.base.join("staging").join("incoming");
    write(&staging.join("one.txt"), b"one\n");
    write(&staging.join("two.txt"), b"two\n");

    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    fs::rename(&staging, f.project_root.join("new-dir")).unwrap();
    let one = wait_for_current_bytes(&f, "new-dir/one.txt", 12_000);
    let two = wait_for_current_bytes(&f, "new-dir/two.txt", 12_000);
    let st = wait_for_state(&f, d.id(), 10_000, "partialCycles >= 1", |s| st_u64(s, "partialCycles") >= 1);
    assert!(one.is_some() && two.is_some(), "a directory notification must bring its files into scope: {st}");
    let lp = last_partial(&st, "p-newdir").expect("the partial pass must publish what it did");
    assert!(lp["scopePaths"].as_u64().unwrap_or(0) >= 1, "the directory was among the paths: {lp}");
    assert!(lp["dirsWalked"].as_u64().unwrap_or(0) >= 1, "its subtree was walked: {lp}");

    // The new directory must be watched now: a change inside it must trigger a pass of its own.
    let before = st_u64(&st, "partialCycles");
    write(&f.project_root.join("new-dir/two.txt"), b"two changed\n");
    let changed = wait_for_current_bytes(&f, "new-dir/two.txt", 12_000);
    let st2 = wait_for_state(&f, d.id(), 10_000, "a second partial pass", |s| st_u64(s, "partialCycles") > before);
    d.stop();
    assert!(changed.is_some(), "a change inside the new directory must be seen (it must be watched): {st2}");
    assert!(st_u64(&st2, "partialCycles") > before, "and it must be notification-driven, not the 60 s interval: {st2}");
    consistent(&f).unwrap();
}

// =============================================================================================
// 11. A folder is deleted: every file recorded under it becomes a delete, in one batch.
// =============================================================================================
#[test]
fn partial_pass_writes_a_batch_of_deletes_for_a_deleted_folder() {
    let mut f = setup(
        "p-rmdir",
        &[("src/a.ts", b"a\n"), ("src/deep/b.ts", b"b\n"), ("src/deep/c.ts", b"c\n"), ("keep.txt", b"k\n")],
    );
    scan_initial(&mut f);
    fs::remove_dir_all(f.project_root.join("src")).unwrap();

    let rep = partial(&mut f, &["src"]);
    assert_eq!(rep.deleted, 3, "every recorded file under the folder: {rep:?}");
    assert_eq!(rep.moved, 0, "nothing was renamed");
    assert_eq!(rep.created, 0);
    let dels = kinds(&f, "delete");
    let paths: BTreeSet<String> = dels.iter().map(|(_, p, _, _)| p.clone()).collect();
    assert_eq!(
        paths,
        ["src/a.ts", "src/deep/b.ts", "src/deep/c.ts"].iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
    );
    let batches: BTreeSet<String> = journal(&f)
        .iter()
        .filter(|e| events::event_type(e) == "delete")
        .map(|e| events::get_str(e, "batchId").unwrap_or_default())
        .collect();
    assert_eq!(batches.len(), 1, "one folder deletion is one batch: {batches:?}");
    let c = cache_of(&f);
    assert!(c.files.keys().all(|k| !k.starts_with("src/")), "nothing under the folder is still tracked: {:?}", c.files.keys());
    assert!(c.files.contains_key("keep.txt"), "the rest of the project is untouched");
    consistent(&f).unwrap();
}

// =============================================================================================
// 12. A file outside the notified set is not processed — and the full pass that follows stores it.
//     This is the boundary of the optimisation, and the reason a lost notification is survivable.
// =============================================================================================
#[test]
fn a_partial_pass_touches_nothing_outside_the_notified_paths() {
    let mut f = setup("p-scope", &[]);
    for i in 0..20 {
        write(&f.project_root.join("src").join(format!("f{i:02}.txt")), format!("v0 {i}\n").as_bytes());
    }
    scan_initial(&mut f);
    let events_before = journal(&f).len();

    // five files change; the notification mentions exactly one of them
    for i in 0..5 {
        write(&f.project_root.join("src").join(format!("f{i:02}.txt")), format!("v1 {i}\n").as_bytes());
    }
    let rep = partial(&mut f, &["src/f00.txt"]);
    assert_eq!(rep.changed, 1, "only the notified file may be read: {rep:?}");
    assert_eq!(rep.created, 0, "{rep:?}");
    assert_eq!(rep.deleted, 0, "an unmentioned file must never be reported as deleted: {rep:?}");
    assert_eq!(rep.tracked_before, 20, "{rep:?}");
    assert_eq!(journal(&f).len(), events_before + 1, "exactly one event was written: {:?}", tail(&f));
    let after = tail(&f);
    assert_eq!((after[0].0.as_str(), after[0].1.as_str()), ("put", "src/f00.txt"));
    for i in 1..5 {
        let rel = format!("src/f{i:02}.txt");
        assert_eq!(puts(&f).iter().filter(|(p, _)| *p == rel).count(), 1, "{rel} must still hold its single old version");
    }
    // the files this pass did not walk are still tracked with their old hash, so nothing is lost
    let c = cache_of(&f);
    assert_eq!(c.files.len(), 20, "the cache must keep the files this pass did not walk: {}", c.files.len());

    // the periodic full pass is what closes the gap
    let rep2 = scan_now(&mut f);
    assert_eq!(rep2.changed, 4, "the other four changed files are found by the full pass: {rep2:?}");
    let state = events::state_at(&journal(&f), i64::MAX, None);
    for i in 0..5 {
        let rel = format!("src/f{i:02}.txt");
        assert_eq!(state.get(&rel).map(|s| s.hash.clone()), Some(hash_of(&f.project_root.join(&rel))), "{rel}");
    }
    assert!(kinds(&f, "delete").is_empty(), "nothing was ever deleted: {:?}", kinds(&f, "delete"));
    consistent(&f).unwrap();
}

// =============================================================================================
// Extra: an unchanged notified path writes nothing — the optimisation must not become a source of
// versions of its own.
// =============================================================================================
#[test]
fn a_partial_pass_over_an_unchanged_path_writes_nothing() {
    let mut f = setup("p-noop", &[("src/a.txt", b"a\n"), ("src/b.txt", b"b\n")]);
    scan_initial(&mut f);
    let before = journal(&f).len();
    let rep = partial(&mut f, &["src/a.txt"]);
    assert_eq!(rep.unchanged, 1, "{rep:?}");
    assert_eq!(rep.created + rep.changed + rep.deleted + rep.moved, 0, "{rep:?}");
    assert_eq!(journal(&f).len(), before, "no event at all: {:?}", tail(&f));
    assert_eq!(cache_of(&f).files.len(), 2, "the cache is unchanged and complete");
    consistent(&f).unwrap();
}

// =============================================================================================
// Extra: the documented switch. With `partialPass=false` a notification still starts a pass, and
// that pass is the ordinary full one — which is also the positive control for `partialCycles`
// being a counter of something real rather than a constant.
// =============================================================================================
#[test]
fn the_partial_pass_can_be_switched_off_and_the_notification_still_starts_a_pass() {
    let mut f = many_files("p-off", 50);
    scan_initial(&mut f);
    set_interval(&f, 60);
    set_config(&f, "partialPass", Value::from(false));
    let mut d = Daemon::start(&f, &[]);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");
    write(&f.project_root.join("src/new.txt"), b"created\n");
    let found = wait_for_current_bytes(&f, "src/new.txt", 12_000);
    let st = wait_for_state(&f, d.id(), 8_000, "triggerCycles >= 1", |s| st_u64(s, "triggerCycles") >= 1);
    d.stop();
    assert!(found.is_some(), "the notification-driven pass must still store the file: {st}");
    assert!(st_u64(&st, "triggerCycles") >= 1, "{st}");
    assert_eq!(st_u64(&st, "partialCycles"), 0, "with partialPass=false the pass is the full one: {st}");
    assert_eq!(st_u64(&st, "periodicCycles"), 1, "and the 60 s interval still explains nothing: {st}");
    consistent(&f).unwrap();
}

// =============================================================================================
// Extra: the pass is reachable from the shipped binary (`pl partial-pass`), which is what makes it
// measurable from outside the process — and what the performance bench drives.
// =============================================================================================
#[test]
fn the_shipped_binary_can_run_a_partial_pass_on_demand() {
    let mut f = setup("p-cli", &[]);
    for i in 0..10 {
        write(&f.project_root.join("src").join(format!("f{i:02}.txt")), format!("v0 {i}\n").as_bytes());
    }
    scan_initial(&mut f);
    write(&f.project_root.join("src/f03.txt"), b"v1 3\n");
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let archive = f.arch.root.to_string_lossy().to_string();

    let out = run_bin(&home, &["--archive", &archive, "partial-pass", "p-cli", "src/f03.txt", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["partial"], Value::Bool(true));
    assert_eq!(v["filesInScope"].as_u64(), Some(1), "{v}");
    assert_eq!(v["changed"].as_u64(), Some(1), "{v}");
    assert_eq!(v["trackedBefore"].as_u64(), Some(10), "the report must say how big the project is: {v}");
    assert!(puts(&f).iter().any(|(p, h)| p == "src/f03.txt" && *h == hash_of(&f.project_root.join("src/f03.txt"))));

    // a path outside the project is not in scope: nothing is read, nothing is written
    let before = journal(&f).len();
    let bad = run_bin(&home, &["--archive", &archive, "partial-pass", "p-cli", "../../etc/passwd"]);
    assert!(bad.status.success(), "an unusable path is simply not in scope: {}", String::from_utf8_lossy(&bad.stdout));
    assert_eq!(journal(&f).len(), before, "and it writes nothing: {:?}", tail(&f));
    consistent(&f).unwrap();
}
