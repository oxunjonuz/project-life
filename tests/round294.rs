//! Round 294 acceptance tests: the **bookkeeping of a partial pass**.
//!
//! Round 293 made a notification-driven pass walk only what was named; it still read the whole
//! journal (three times) and parsed and rewrote the whole state cache, which was 71 ms of its 91 ms
//! on a 10 000-file project. Round 294 changes *where the state is read from and written to* and
//! nothing about what a pass decides.
//!
//! So these tests come in three kinds, and each kind is there because the others cannot see its
//! failure:
//!
//! 1. **The decisions.** Create, modify, delete, rename inside a folder and between folders — run
//!    through a partial pass, and compared with the state the same changes produce through full
//!    passes on a twin fixture. A pass that reads less and decides differently fails here.
//! 2. **What it read.** The cache base file is byte-identical after a partial pass and only the
//!    delta grew; a partial pass completes while the journal is unreadable, where a full pass fails.
//! 3. **The seams.** A crash between the base write and the delta reset, a tail that lies about the
//!    last sequence number, a legacy cache, a delta that grows past its cap.
//!
//! The helpers are duplicated from `round293.rs` on purpose: integration tests are separate crates,
//! and that file already proves its own round.

use projectlife::archive::{iso_ms, Archive, Project};
use projectlife::cache::{self, CacheStore};
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
    pub home: PathBuf,
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

fn setup_fixture(name: &str, files: &[(&str, &[u8])]) -> Fixture {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("projectlife-294-{}-{}-{}", name, std::process::id(), id));
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
    fs::create_dir_all(base.join("home")).unwrap();
    let home = base.join("home");
    Fixture { base, arch, project, project_root, home }
}

fn setup(name: &str, files: &[(&str, &[u8])]) -> Fixture {
    let mut f = setup_fixture(name, files);
    scan_initial(&mut f);
    f
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

fn partial(f: &mut Fixture, paths: &[&str]) -> scan::ScanReport {
    let ps: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
    f.project = reload(f);
    scan::scan_project_partial(&f.arch, &mut f.project, &ps, "changed").unwrap()
}

fn journal(f: &Fixture) -> Vec<events::Ev> {
    events::load_journal(&f.project.dir).unwrap_or(events::Journal { events: Vec::new(), trailing_partial: false }).events
}

/// The tracked state per path, as `path -> hash` — the thing a pass decides.
fn state_hashes(f: &Fixture) -> BTreeMap<String, String> {
    events::state_at(&journal(f), i64::MAX, None)
        .into_iter()
        .map(|(p, st)| (p, if st.kind == "symlink" { format!("link:{}", st.target.unwrap_or_default()) } else { st.hash }))
        .collect()
}

fn kinds(f: &Fixture, kind: &str) -> Vec<(String, String, String, String)> {
    journal(f)
        .iter()
        .filter(|e| events::event_type(e) == kind)
        .map(|e| {
            (
                events::get_str(e, "path").unwrap_or_default(),
                events::get_str(e, "from").unwrap_or_default(),
                events::get_str(e, "to").unwrap_or_default(),
                events::get_str(e, "batchId").unwrap_or_default(),
            )
        })
        .collect()
}

fn cache_dir(f: &Fixture) -> PathBuf {
    f.project.dir.join("cache")
}

fn sha_of(p: &Path) -> String {
    store::sha256_bytes(&fs::read(p).unwrap_or_default())
}

fn run_bin(f: &Fixture, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", &f.home);
    c.args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("the shipped binary must be runnable")
}

fn cli_partial(f: &Fixture, paths: &[&str]) -> Value {
    let mut args: Vec<String> =
        vec!["--archive".into(), f.arch.root.to_string_lossy().to_string(), "partial-pass".into(), f.project.name.clone()];
    for p in paths {
        args.push((*p).to_string());
    }
    args.push("--json".into());
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_bin(f, &refs, &[]);
    assert!(
        out.status.success(),
        "partial-pass failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("partial-pass --json must print JSON")
}

// ---------------------------------------------------------------------------------------------
// 1. The decisions: the same changes, through partial passes and through full passes.
// ---------------------------------------------------------------------------------------------

/// The whole sequence a real watch would produce: create, modify, delete, rename inside a folder and
/// between folders. Fixture A is driven by partial passes with exactly the paths inotify would name;
/// fixture B is driven by ordinary full passes. Their **states must be identical** — and if a partial
/// pass gets a decision wrong (misses a rename, keeps a deleted path, forgets a delta it wrote), this
/// is where it shows.
#[test]
fn partial_passes_reach_the_same_state_as_full_passes() {
    let files: &[(&str, &[u8])] = &[("d1/a.txt", b"one\n"), ("d2/keep.txt", b"keep\n"), ("d3/x.txt", b"x\n")];
    let mut a = setup("pdiff", files);
    let mut b = setup_fixture("pdiff2", files);
    let mut b2 = Project::from_dir(&b.project.dir).unwrap();
    b.project = b2;
    let opts = ScanOptions { reason: "initial".into(), deep: true, with_initial_snapshot: true, ..Default::default() };
    scan::scan_project(&b.arch, &mut b.project, &opts).unwrap();
    b2 = reload(&b);
    b.project = b2;

    // 1. create a new file in d1
    write(&a.project_root.join("d1/new.txt"), b"brand new\n");
    write(&b.project_root.join("d1/new.txt"), b"brand new\n");
    partial(&mut a, &["d1"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 2. modify it (a new version)
    write(&a.project_root.join("d1/new.txt"), b"second\n");
    write(&b.project_root.join("d1/new.txt"), b"second\n");
    partial(&mut a, &["d1/new.txt"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 3. rename inside the same folder — the notification names the folder
    fs::rename(a.project_root.join("d1/new.txt"), a.project_root.join("d1/renamed.txt")).unwrap();
    fs::rename(b.project_root.join("d1/new.txt"), b.project_root.join("d1/renamed.txt")).unwrap();
    partial(&mut a, &["d1"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 4. rename between folders — a real watch names both directories
    fs::rename(a.project_root.join("d1/renamed.txt"), a.project_root.join("d2/moved.txt")).unwrap();
    fs::rename(b.project_root.join("d1/renamed.txt"), b.project_root.join("d2/moved.txt")).unwrap();
    partial(&mut a, &["d1", "d2"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 5. modify it where it now lives
    write(&a.project_root.join("d2/moved.txt"), b"moved and changed\n");
    write(&b.project_root.join("d2/moved.txt"), b"moved and changed\n");
    partial(&mut a, &["d2"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 6. delete a file whose directory is named
    fs::remove_file(a.project_root.join("d3/x.txt")).unwrap();
    fs::remove_file(b.project_root.join("d3/x.txt")).unwrap();
    partial(&mut a, &["d3"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    // 7. delete a file that is named itself (a watch names the file on IN_DELETE)
    fs::remove_file(a.project_root.join("d1/a.txt")).unwrap();
    fs::remove_file(b.project_root.join("d1/a.txt")).unwrap();
    partial(&mut a, &["d1/a.txt"]);
    b.project = reload(&b);
    scan::scan_project(&b.arch, &mut b.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();

    assert_eq!(
        state_hashes(&a),
        state_hashes(&b),
        "the partial passes and the full passes must agree on the state of every path"
    );
    // and the fixture really did exercise the kinds: the state alone would also agree if nothing
    // had happened.
    let sa = state_hashes(&a);
    assert!(sa.contains_key("d2/moved.txt"), "{sa:?}");
    assert!(!sa.contains_key("d3/x.txt"), "the deleted file must be gone: {sa:?}");
    assert!(!sa.contains_key("d1/a.txt"), "{sa:?}");
    assert!(!sa.contains_key("d1/new.txt"), "the renamed-away path must be gone: {sa:?}");

    // The archive must be consistent on both sides: every referenced blob present, no dangling one.
    for f in [&a, &b] {
        let st = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
        let mut referenced: BTreeSet<String> = BTreeSet::new();
        for e in journal(f) {
            if events::event_type(&e) == "put" {
                if let Some(h) = events::get_str(&e, "hash") {
                    referenced.insert(h);
                }
            }
        }
        let present: BTreeSet<String> = st.list_all().into_iter().map(|(h, _)| h).collect();
        assert!(referenced.is_subset(&present), "referenced blobs missing in {}", f.project.name);
        assert!(present.is_subset(&referenced), "dangling blobs in {}", f.project.name);
    }
}

/// A path created by one partial pass lives only in the delta. Everything the next pass has to know
/// about it — that it exists, what its content was, how many paths are tracked — has to come back
/// out of that delta: this test walks a delta-only path through a modification, a rename and a
/// deletion, and reads the journal after each step.
#[test]
fn a_path_that_lives_only_in_the_delta_is_still_known() {
    let mut f = setup("deltaonly", &[("d/keep.txt", b"keep\n")]);

    write(&f.project_root.join("d/fresh.txt"), b"first\n");
    let r1 = partial(&mut f, &["d"]);
    assert_eq!(r1.created, 1, "the first pass must store it as new");
    assert_eq!(r1.tracked_before, 1, "one path was tracked before it");
    // The new path lives only in the delta, and the count the pass reports has to include it: the
    // base file still holds one entry.
    let base_lines = fs::read_to_string(cache_dir(&f).join("base.jsonl")).unwrap().lines().count();
    assert_eq!(base_lines, 1, "the base must not have been rewritten to hold the new path");
    let r1b = partial(&mut f, &["d/keep.txt"]);
    assert_eq!(r1b.tracked_before, 2, "the delta's own record says a second path is tracked now");
    assert_eq!(state_hashes(&f).len(), 2, "and the journal agrees");

    // modify: the previous version must be seen, so this is a version, not a first sight
    write(&f.project_root.join("d/fresh.txt"), b"second\n");
    let r2 = partial(&mut f, &["d/fresh.txt"]);
    assert_eq!((r2.created, r2.changed, r2.deleted), (0, 1, 0), "the delta must make it a change");
    assert_eq!(
        kinds(&f, "put").iter().filter(|(p, _, _, _)| p == "d/fresh.txt").count(),
        2,
        "exactly two versions: the delta-only path must not be re-created"
    );

    // rename it: the identity is in the delta, so the move must be recognised
    fs::rename(f.project_root.join("d/fresh.txt"), f.project_root.join("d/fresh2.txt")).unwrap();
    let r3 = partial(&mut f, &["d"]);
    assert_eq!((r3.moved, r3.created, r3.deleted), (1, 0, 0), "a delta-only path must be renameable: {r3:?}");
    let moves = kinds(&f, "move");
    assert!(moves.iter().any(|(_, from, to, _)| from == "d/fresh.txt" && to == "d/fresh2.txt"), "{moves:?}");

    // delete it: the deletion must be written, not skipped for "not tracked"
    fs::remove_file(f.project_root.join("d/fresh2.txt")).unwrap();
    let r4 = partial(&mut f, &["d"]);
    assert_eq!(r4.deleted, 1, "a delta-only path must be deletable: {r4:?}");
    assert!(!state_hashes(&f).contains_key("d/fresh2.txt"));

    // the count of tracked paths must still be right (the delta's own records say what they did)
    let tracked = state_hashes(&f).len();
    let r5 = partial(&mut f, &["d/keep.txt"]);
    assert_eq!(r5.tracked_before, tracked, "the tracked count must equal the journal's own state");

    // The tombstone must also keep the deleted path out of the candidate set: it is still in the
    // base file (the base is not rewritten), and a pass over that directory must not read it out of
    // the base and report the deletion a second time.
    assert_eq!(kinds(&f, "delete").len(), 1, "one deletion so far");
    let r6 = partial(&mut f, &["d"]);
    assert_eq!(r6.deleted, 0, "the path is already forgotten: {r6:?}");
    assert_eq!(kinds(&f, "delete").len(), 1, "and no second delete event may be written");
}

// ---------------------------------------------------------------------------------------------
// 2. What the pass read and wrote.
// ---------------------------------------------------------------------------------------------

/// The cache is two files: a sorted base and an append-only delta. A partial pass must leave the
/// base **byte-identical** and only append to the delta — and a full pass must do the opposite
/// (fold the delta in and rewrite the base), or this test could not tell "no rewrite" from "nothing
/// ever rewrites".
#[test]
fn a_partial_pass_does_not_rewrite_the_cache_base() {
    let mut f = setup("inplace", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n"), ("e/c.txt", b"c\n")]);
    let base = cache_dir(&f).join("base.jsonl");
    let delta = cache_dir(&f).join("delta.jsonl");
    let base_before = sha_of(&base);
    let delta_before = fs::read_to_string(&delta).unwrap_or_default().lines().count();

    write(&f.project_root.join("d/a.txt"), b"a2\n");
    let r = partial(&mut f, &["d/a.txt"]);
    assert_eq!(r.cache_base_rewrites, 0, "a partial pass must not rewrite the base");
    assert_eq!(sha_of(&base), base_before, "the base file must not change at all");
    let delta_after = fs::read_to_string(&delta).unwrap();
    assert_eq!(
        delta_after.lines().count(),
        delta_before + 1,
        "exactly one delta record for one changed path:\n{delta_after}"
    );
    assert!(delta_after.contains("\"path\":\"d/a.txt\""), "{delta_after}");

    // The cache and the journal must agree about that path — the record is not just written, it is
    // the right one.
    let st = state_hashes(&f);
    let entries = cache::materialize(&f.project).unwrap();
    let e = entries.get("d/a.txt").expect("the changed path must be in the cache");
    assert_eq!(e.hash, st["d/a.txt"], "the cache entry must match the journal's state");

    // Positive control: a full pass DOES rewrite the base and empties the delta.
    f.project = reload(&f);
    scan::scan_project(&f.arch, &mut f.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();
    assert_eq!(fs::read_to_string(&delta).unwrap().lines().count(), 0, "a full pass folds the delta in");
    assert_eq!(fs::read_to_string(&cache_dir(&f).join("base.meta.json")).unwrap().contains("\"cause\":\"full\""), true);
}

/// A partial pass must not read the journal, and that is not a promise about a counter: the journal
/// is made **unreadable** — an older month file replaced by a directory of the same name — and the
/// pass has to store its change anyway. The full pass on the same archive has to fail, which is the
/// control that proves the injection really does break a reader of the journal.
#[test]
fn a_partial_pass_works_while_the_journal_is_unreadable_and_a_full_pass_does_not() {
    let mut f = setup("nojournal", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    // An older month file that no reader can read: a directory with the name of a journal file.
    let broken = f.project.events_dir().join("2020-01.jsonl");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("not-a-journal"), b"x").unwrap();

    // The control: reading the journal in full fails, and a full pass therefore fails too.
    assert!(events::load_journal(&f.project.dir).is_err(), "the injection must break a full read");
    f.project = reload(&f);
    let full = scan::scan_project(&f.arch, &mut f.project, &ScanOptions { reason: "observed".into(), ..Default::default() });
    assert!(full.is_err(), "a full pass must notice that the journal is unreadable");

    // The partial pass, on the same archive, stores the change. Run through the shipped binary so
    // the byte counters belong to one process and one pass.
    write(&f.project_root.join("d/a.txt"), b"a2\n");
    let doc = cli_partial(&f, &["d/a.txt"]);
    assert_eq!(doc["changed"], 1, "{doc}");
    assert_eq!(doc["journalFullBytesRead"], 0, "a partial pass must not read the journal: {doc}");
    assert!(doc["journalTailBytesRead"].as_u64().unwrap_or(0) > 0, "it must still consult the journal's tail: {doc}");
    assert_eq!(doc["cacheBaseRewrites"], 0, "{doc}");

    // And the change is really in the journal: read it back with the broken file out of the way.
    fs::remove_dir_all(&broken).unwrap();
    let st = state_hashes(&f);
    assert_eq!(st["d/a.txt"], store::sha256_bytes(b"a2\n"), "the version must be stored: {st:?}");
}

/// The bytes a pass reads out of the state must not grow with the size of the project. On a small
/// fixture the absolute numbers are meaningless, so the test measures the *ratio* against the base
/// file itself: a pass that touched one path must read a small fraction of it.
#[test]
fn a_partial_pass_reads_only_the_part_of_the_cache_it_needs() {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..1200 {
        files.push((format!("d{:02}/f{:04}.txt", i / 20, i), format!("content {i}\n").into_bytes()));
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, d)| (p.as_str(), d.as_slice())).collect();
    let mut f = setup("readpart", &refs);
    let base_bytes = fs::metadata(cache_dir(&f).join("base.jsonl")).unwrap().len();
    assert!(base_bytes > 20_000, "the fixture must have a base worth not reading: {base_bytes} B");

    write(&f.project_root.join("d00/f0000.txt"), b"changed\n");
    let r = partial(&mut f, &["d00/f0000.txt"]);
    assert!(
        r.cache_bytes_read * 4 < base_bytes,
        "a one-file pass read {} B of a {} B base — that is not a partial read",
        r.cache_bytes_read,
        base_bytes
    );
    // The journal counter is a counter of the whole process, and this test shares its process with
    // the others: the claim "a partial pass reads no journal in full" is asserted where it can be
    // measured — in `a_partial_pass_works_while_the_journal_is_unreadable_and_a_full_pass_does_not`,
    // which runs the shipped binary in its own process.
}

// ---------------------------------------------------------------------------------------------
// 3. The seams.
// ---------------------------------------------------------------------------------------------

/// The one crash window the format has: the new base is in place and the delta still holds records
/// already folded into it. Folding is idempotent, so a reader that applies them again must reach the
/// same state — this kills a real process at that exact point and then compares against a twin that
/// never crashed.
#[test]
fn a_crash_between_the_base_write_and_the_delta_reset_costs_nothing() {
    let mut f = setup("crashfold", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    // Build a delta: two partial passes that change two different paths.
    write(&f.project_root.join("d/a.txt"), b"a2\n");
    partial(&mut f, &["d/a.txt"]);
    write(&f.project_root.join("d/b.txt"), b"b2\n");
    partial(&mut f, &["d/b.txt"]);
    let delta_lines = fs::read_to_string(cache_dir(&f).join("delta.jsonl")).unwrap().lines().count();
    assert!(delta_lines >= 2, "the delta must hold records to fold: {delta_lines}");

    // A twin with the same history, to compare against.
    let mut twin = setup("crashfold2", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    write(&twin.project_root.join("d/a.txt"), b"a2\n");
    partial(&mut twin, &["d/a.txt"]);
    write(&twin.project_root.join("d/b.txt"), b"b2\n");
    partial(&mut twin, &["d/b.txt"]);
    twin.project = reload(&twin);
    scan::scan_project(&twin.arch, &mut twin.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();
    let want = state_hashes(&twin);
    let want_puts = kinds(&twin, "put").len();

    // Kill a real process at each side of the window: between the base write and the delta reset
    // (records that are already folded in are still on disk), and right after the reset.
    for phase in ["cache_base_written", "cache_delta_reset"] {
        let out = run_bin(
            &f,
            &["--archive", &f.arch.root.to_string_lossy(), "scan-once", &f.project.name],
            &[("PROJECTLIFE_CRASH_AFTER", phase)],
        );
        assert!(
            out.status.code().is_none(),
            "phase {phase}: the process must have been killed by a signal: {:?}",
            out.status.code()
        );

        // The archive is usable immediately, and the state is the twin's.
        f.project = reload(&f);
        scan::scan_project(&f.arch, &mut f.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();
        assert_eq!(state_hashes(&f), want, "phase {phase}: the state must equal the twin's");
        assert_eq!(kinds(&f, "put").len(), want_puts, "phase {phase}: no version may be stored twice");
    }
    let entries = cache::materialize(&f.project).unwrap();
    let twin_entries = cache::materialize(&twin.project).unwrap();
    assert_eq!(entries.len(), twin_entries.len(), "the tracked count must not drift");
    for (p, e) in &twin_entries {
        assert_eq!(entries.get(p).map(|x| x.hash.clone()), Some(e.hash.clone()), "entry {p}");
    }
}

/// A tail that lies is not trusted: `cache/tail.json` claims a sequence number far ahead of the
/// journal, and the next pass must not allocate a colliding one. Deleting the tail must be just as
/// harmless.
#[test]
fn a_tail_that_disagrees_with_the_journal_is_rebuilt_not_trusted() {
    let mut f = setup("lyingtail", &[("d/a.txt", b"a\n")]);
    let tail_path = f.project.dir.join("cache").join("tail.json");
    let before: Value = serde_json::from_str(&fs::read_to_string(&tail_path).unwrap()).unwrap();
    let real_seq = before["seqLast"].as_u64().unwrap();
    let mut lying = before.clone();
    lying["seqLast"] = Value::from(real_seq + 5000);
    fs::write(&tail_path, serde_json::to_string(&lying).unwrap()).unwrap();

    write(&f.project_root.join("d/a.txt"), b"a2\n");
    let r = partial(&mut f, &["d/a.txt"]);
    assert_eq!(r.changed, 1);
    let seqs: Vec<u64> = journal(&f).iter().map(events::seq_of).collect();
    let unique: BTreeSet<u64> = seqs.iter().cloned().collect();
    assert_eq!(seqs.len(), unique.len(), "no two events may share a sequence number");
    let max = *unique.iter().max().unwrap();
    assert!(max < real_seq + 5000, "the lie must not be believed: max seq {max}");
    let now: Value = serde_json::from_str(&fs::read_to_string(&tail_path).unwrap()).unwrap();
    assert_eq!(now["seqLast"].as_u64().unwrap(), max, "the tail must have been rebuilt from the journal");

    // A missing tail is not an error either.
    fs::remove_file(&tail_path).unwrap();
    write(&f.project_root.join("d/a.txt"), b"a3\n");
    let r2 = partial(&mut f, &["d/a.txt"]);
    assert_eq!(r2.changed, 1);
    let seqs2: Vec<u64> = journal(&f).iter().map(events::seq_of).collect();
    let unique2: BTreeSet<u64> = seqs2.iter().cloned().collect();
    assert_eq!(seqs2.len(), unique2.len(), "and still no duplicate sequence numbers");
    assert!(tail_path.is_file(), "the tail must be written again");
}

/// The cache of an archive written by the previous format (one `state.json`) must be migrated, not
/// ignored: the same entries, the base in the new files, and the old file kept aside.
#[test]
fn a_legacy_cache_is_migrated_and_its_entries_survive() {
    let mut f = setup("legacy", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n"), ("e/c.txt", b"c\n")]);
    let entries_before = cache::materialize(&f.project).unwrap();
    assert!(entries_before.len() >= 3);

    // Rebuild the format-1 layout by hand: one object, and none of the new files.
    let mut files = serde_json::Map::new();
    for (p, e) in &entries_before {
        files.insert(p.clone(), serde_json::to_value(e).unwrap());
    }
    let legacy = json!({"version": 1, "files": Value::Object(files)});
    let dir = cache_dir(&f);
    for name in ["base.jsonl", "base.meta.json", "delta.jsonl"] {
        let _ = fs::remove_file(dir.join(name));
    }
    fs::write(dir.join("state.json"), serde_json::to_string(&legacy).unwrap()).unwrap();

    // An ordinary full pass migrates it.
    f.project = reload(&f);
    let r = scan::scan_project(&f.arch, &mut f.project, &ScanOptions { reason: "observed".into(), ..Default::default() }).unwrap();
    assert!(r.tracked_before >= 3, "the migrated entries must be visible to the pass: {r:?}");
    assert!(!dir.join("state.json").is_file(), "the legacy file must be moved aside");
    let kept: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("state.json.v1-")).unwrap_or(false))
        .collect();
    assert_eq!(kept.len(), 1, "the old file must be kept for inspection: {kept:?}");
    assert!(dir.join("base.jsonl").is_file());

    let entries_after = cache::materialize(&f.project).unwrap();
    assert_eq!(entries_after.len(), entries_before.len(), "no entry may be lost in the migration");
    for (p, e) in &entries_before {
        assert_eq!(entries_after.get(p).map(|x| x.hash.clone()), Some(e.hash.clone()), "entry {p} after migration");
    }
    assert_eq!(state_hashes(&f).len(), entries_after.len(), "and the cache must match the journal");
}

/// The delta is capped in bytes; past the cap a pass folds it into a new base. That must be counted,
/// must leave an empty delta, and — the only thing that matters — must not change any decision.
#[test]
fn a_delta_past_its_cap_is_folded_and_the_state_is_unchanged() {
    let mut f = setup("compact", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    let mut cfg = Archive::open(&f.arch.root).unwrap().config;
    cfg.set("cacheDeltaMaxBytes", Value::from(400));
    cfg.save(&f.arch.root).unwrap();
    f.arch = Archive::open(&f.arch.root).unwrap();

    let mut compactions = 0;
    for i in 0..12 {
        write(&f.project_root.join("d/a.txt"), format!("version {i}\n").as_bytes());
        let r = partial(&mut f, &["d/a.txt"]);
        compactions += r.cache_compactions;
        assert_eq!(r.cache_base_rewrites, r.cache_compactions, "only a compaction rewrites the base");
    }
    assert!(compactions >= 1, "the cap must have been reached ({compactions} compactions)");
    let delta_bytes = fs::metadata(cache_dir(&f).join("delta.jsonl")).map(|m| m.len()).unwrap_or(0);
    assert!(delta_bytes <= 400, "the delta must have been folded: {delta_bytes} B left");

    // The state after all that must still be exactly what the journal says.
    let entries = cache::materialize(&f.project).unwrap();
    let st = state_hashes(&f);
    assert_eq!(entries.len(), st.len(), "the cache and the journal must agree on the set of paths");
    for (p, h) in &st {
        assert_eq!(entries.get(p).map(|e| e.hash.clone()), Some(h.clone()), "path {p}");
    }
}

/// A cache that was deleted is not a correctness problem: the pass rebuilds it from the journal
/// (the documented slow path), stores the change, and no version is lost or duplicated.
#[test]
fn a_deleted_cache_is_rebuilt_and_the_change_is_still_stored() {
    let mut f = setup("nocache", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    let before_puts = kinds(&f, "put").len();
    fs::remove_dir_all(cache_dir(&f)).unwrap();

    write(&f.project_root.join("d/a.txt"), b"a2\n");
    let r = partial(&mut f, &["d/a.txt"]);
    assert!(r.cache_rebuilt, "the pass must say that it rebuilt the cache: {r:?}");
    assert_eq!(r.changed, 1, "the change must still be stored: {r:?}");
    assert_eq!(r.tracked_before, 2, "the rebuilt cache must know both paths: {r:?}");
    assert_eq!(kinds(&f, "put").len(), before_puts + 1, "exactly one new version");

    // The rebuilt cache must be complete and correct, not just present.
    let entries = cache::materialize(&f.project).unwrap();
    assert_eq!(entries.len(), state_hashes(&f).len());
    assert_eq!(entries.get("d/a.txt").map(|e| e.hash.clone()), Some(store::sha256_bytes(b"a2\n")));

    // And the next partial pass must be back on the cheap path.
    write(&f.project_root.join("d/b.txt"), b"b2\n");
    let r2 = partial(&mut f, &["d/b.txt"]);
    assert!(!r2.cache_rebuilt, "the second pass must not need a rebuild: {r2:?}");
    assert_eq!(r2.cache_base_rewrites, 0);
}

/// The store on its own: what it answers for a path must be the newest record that mentions it, and
/// a tombstone must hide the base entry. This is the seam the whole round rests on, tested without a
/// pass around it.
#[test]
fn the_cache_store_reads_the_delta_over_the_base() {
    let f = setup("store", &[("d/a.txt", b"a\n")]);
    let dir = f.project.dir.clone();
    let mut store = CacheStore::open(&f.project, false, 0).unwrap();
    assert_eq!(store.tracked_before(), 1);
    let base_entry = store.get("d/a.txt").expect("the base entry");
    assert_eq!(base_entry.hash, store::sha256_bytes(b"a\n"));

    // A newer version, staged: the store must answer with it, not with the base.
    let mut newer = base_entry.clone();
    newer.hash = "f".repeat(64);
    store.set("d/a.txt", newer.clone());
    assert_eq!(store.get("d/a.txt").map(|e| e.hash), Some(newer.hash.clone()));
    // A second path: the count follows the transitions the records carry.
    store.set("d/b.txt", base_entry.clone());
    assert_eq!(store.tracked_before(), 1, "the count at the start of the pass does not move");
    store.del("d/a.txt");
    assert!(store.get("d/a.txt").is_none(), "a tombstone must hide the base entry");
    store.commit(&f.project).unwrap();

    // Read it back from disk in a fresh store: the delta is the truth for those paths.
    let p = Project::from_dir(&dir).unwrap();
    let store2 = CacheStore::open(&p, false, 0).unwrap();
    assert!(store2.get("d/a.txt").is_none(), "the tombstone must survive a reopen");
    assert_eq!(store2.get("d/b.txt").map(|e| e.hash), Some(base_entry.hash));
    assert_eq!(store2.tracked_before(), 1, "one path left: the delta says so");
    let entries = cache::materialize(&p).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries.contains_key("d/b.txt"));
}

/// A crash can leave the last journal line without its newline, and what is there can be either a
/// torn line or a complete record. Both must be handled, and the journal must stay readable: closing
/// a torn line (or joining the next record to it) would put a line no reader can parse in the middle
/// of the journal, which the journal cannot repair — it only tolerates a torn *last* line.
#[test]
fn a_half_written_last_line_is_not_joined_by_the_next_append() {
    let mut f = setup("halfwritten", &[("d/a.txt", b"a\n")]);
    let month = fs::read_dir(f.project.events_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .expect("the journal file");
    let before = journal(&f);
    assert!(before.len() >= 2);

    // Case 1 — a complete record whose newline never landed: it is a record, so it must survive.
    let mut text = fs::read_to_string(&month).unwrap();
    let complete = "{\"seq\":900,\"ts\":1,\"type\":\"note\",\"text\":\"kept\"}";
    text.push_str(complete);
    fs::write(&month, text.as_bytes()).unwrap();
    write(&f.project_root.join("d/a.txt"), b"a2\n");
    let r = partial(&mut f, &["d/a.txt"]);
    assert_eq!(r.changed, 1, "{r:?}");
    let raw = fs::read_to_string(&month).unwrap();
    assert!(raw.contains(&format!("{complete}\n")), "a complete record must be kept and closed:\n{raw}");
    let after = journal(&f);
    assert!(after.iter().any(|e| events::seq_of(e) == 900), "the complete record must be readable");
    assert_eq!(after.len(), before.len() + 2, "the kept record and the new event");

    // Case 2 — a torn line: it was never a record, so it is cut off, and the journal stays readable.
    let mut text = fs::read_to_string(&month).unwrap();
    text.push_str("{\"seq\":901,\"ts\":1,\"type\":\"put\",\"path\":\"d/half");
    fs::write(&month, text.as_bytes()).unwrap();
    assert!(events::load_journal(&f.project.dir).is_ok(), "a torn last line is tolerated");
    let before = journal(&f);
    write(&f.project_root.join("d/a.txt"), b"a3\n");
    let r = partial(&mut f, &["d/a.txt"]);
    assert_eq!(r.changed, 1, "{r:?}");

    let raw = fs::read_to_string(&month).unwrap();
    assert!(!raw.contains("d/half"), "the torn bytes must not be left as a line in the journal:\n{raw}");
    let after = journal(&f);
    assert_eq!(after.len(), before.len() + 1, "the journal must be readable, with the new event and every old one");
    let seqs: Vec<u64> = after.iter().map(events::seq_of).collect();
    let unique: BTreeSet<u64> = seqs.iter().cloned().collect();
    assert_eq!(seqs.len(), unique.len(), "the appended event must not reuse a sequence number");
    assert_eq!(state_hashes(&f)["d/a.txt"], store::sha256_bytes(b"a3\n"));
}

/// A deleted path stays in the base file (a partial pass does not rewrite the base), so the delta's
/// tombstone is the only thing that keeps it out of the next pass's candidate set. Without it the
/// next pass over that directory would read the path out of the base, find it missing from disk and
/// write the deletion a second time — an event for something the journal already forgot.
#[test]
fn a_tombstone_keeps_a_deleted_path_out_of_the_next_pass() {
    let mut f = setup("tombstone", &[("d/a.txt", b"a\n"), ("d/b.txt", b"b\n")]);
    let base_lines = fs::read_to_string(cache_dir(&f).join("base.jsonl")).unwrap().lines().count();
    assert_eq!(base_lines, 2, "both files start in the base");

    fs::remove_file(f.project_root.join("d/b.txt")).unwrap();
    let r1 = partial(&mut f, &["d"]);
    assert_eq!(r1.deleted, 1, "the first pass sees the deletion: {r1:?}");
    assert_eq!(kinds(&f, "delete").len(), 1);
    // the base is untouched, and the deletion exists only as a record in the delta
    assert_eq!(
        fs::read_to_string(cache_dir(&f).join("base.jsonl")).unwrap().lines().count(),
        2,
        "the base must still hold the deleted path's entry"
    );
    assert!(fs::read_to_string(cache_dir(&f).join("delta.jsonl")).unwrap().contains("\"op\":\"del\""));

    // Nothing changed since. A pass over the same directory must write nothing: the path is already
    // forgotten, and the base's copy of it is not news.
    let r2 = partial(&mut f, &["d"]);
    assert_eq!((r2.deleted, r2.created, r2.changed), (0, 0, 0), "a pass over an unchanged directory writes nothing: {r2:?}");
    assert_eq!(kinds(&f, "delete").len(), 1, "and no second delete event");
    assert_eq!(r2.tracked_before, 1, "one path is tracked");

    // And the state is still what the journal says.
    let entries = cache::materialize(&f.project).unwrap();
    let st = state_hashes(&f);
    assert_eq!(entries.len(), st.len(), "the cache and the journal agree on the set of paths");
    assert!(!st.contains_key("d/b.txt"));
}
