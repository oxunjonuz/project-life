//! Acceptance tests on a real filesystem. No mocked filesystem anywhere: every scenario creates
//! real files, real archives and real processes, because that is where the promise lives.

use projectlife::archive::{Archive, Project};
use projectlife::events;
use projectlife::filters::FilterConfig;
use projectlife::lifecycle;
use projectlife::restore::{self, RestoreOptions};
use projectlife::scan::{self, ScanOptions};
use projectlife::store::{self, Store};
use std::collections::BTreeMap;
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
    let base = std::env::temp_dir().join(format!("projectlife-test-{}-{}-{}", name, std::process::id(), id));
    let _ = fs::remove_dir_all(&base);
    let project_root = base.join("project");
    fs::create_dir_all(&project_root).unwrap();
    for (rel, data) in files {
        write(&project_root.join(rel), data);
    }
    let archive_root = base.join("archive");
    let mut arch = Archive::create(&archive_root).unwrap();
    // Tests run on a nearly full shared volume: keep the disk thresholds out of the way.
    arch.config.set("stopFreePercent", serde_json::Value::from(0));
    arch.config.set("warnFreePercent", serde_json::Value::from(0));
    arch.config.save(&archive_root).unwrap();
    let dir = arch.projects_dir().join("00000000-0000-4000-8000-000000000001");
    fs::create_dir_all(&dir).unwrap();
    let meta = serde_json::json!({
        "schemaVersion": 1,
        "projectId": "00000000-0000-4000-8000-000000000001",
        "name": name,
        "projectRoot": project_root.to_string_lossy(),
        "createdAt": projectlife::archive::iso_ms(projectlife::util::now_ms()),
        "historyStartsAt": projectlife::archive::iso_ms(projectlife::util::now_ms()),
        "profile": "source",
        "settings": {},
        "state": "active",
        "lastSeq": 0
    });
    fs::write(dir.join("project.json"), serde_json::to_string_pretty(&meta).unwrap()).unwrap();
    let project = Project::from_dir(&dir).unwrap();
    Fixture { base, arch, project, project_root }
}

fn scan_initial(f: &mut Fixture) -> scan::ScanReport {
    let opts = ScanOptions {
        reason: "initial".into(),
        deep: true,
        verbose_filters: false,
        dry_run: false,
        with_initial_snapshot: true,
        count_skipped: false,
        scope: None,
    };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap()
}

fn scan_now(f: &mut Fixture) -> scan::ScanReport {
    let opts = ScanOptions { reason: "observed".into(), deep: false, ..Default::default() };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap()
}

fn restore_to(f: &Fixture, at: i64, to: &Path) -> restore::RestoreReport {
    let opts = RestoreOptions {
        at,
        at_label: "test".into(),
        paths: Vec::new(),
        to: Some(to.to_path_buf()),
        into_project: false,
        clean: false,
        preview: false,
        missing: false,
    };
    let plan = restore::plan(&f.arch, &f.project, &opts).unwrap();
    restore::execute(&f.project, &plan, &opts).unwrap()
}

fn hash_of(path: &Path) -> String {
    store::sha256_bytes(&fs::read(path).unwrap())
}

fn state_now(f: &Fixture) -> BTreeMap<String, events::FileSt> {
    let j = events::load_journal(&f.project.dir).unwrap();
    events::state_at(&j.events, i64::MAX, None)
}

// AT-01 / AT-02 / AT-06: initial snapshot, five versions, byte-exact restore.
#[test]
fn at02_five_versions_restore_byte_exact() {
    let mut f = setup("five", &[("src/app.ts", b"v0\n")]);
    scan_initial(&mut f);
    for i in 1..=5 {
        write(&f.project_root.join("src/app.ts"), format!("v{i}\n").as_bytes());
        scan_now(&mut f);
    }
    let j = events::load_journal(&f.project.dir).unwrap();
    let puts = j.events.iter().filter(|e| events::event_type(e) == "put").count();
    assert_eq!(puts, 6, "initial snapshot plus five versions");
    for i in 1..=5 {
        let marker = format!("v{i}\n");
        // find the moment right after the i-th version was observed
        let ts = j
            .events
            .iter()
            .filter(|e| events::event_type(e) == "put" && events::get_str(e, "path").as_deref() == Some("src/app.ts"))
            .nth(i)
            .map(events::ts_of)
            .unwrap();
        let out = f.base.join(format!("out{i}"));
        restore_to(&f, ts, &out);
        assert_eq!(fs::read(out.join("src/app.ts")).unwrap(), marker.as_bytes());
    }
}

// AT-03: a file saved without changes creates no version.
#[test]
fn at03_unchanged_file_makes_no_version() {
    let mut f = setup("unchanged", &[("a.txt", b"same\n")]);
    scan_initial(&mut f);
    let before = events::load_journal(&f.project.dir).unwrap().events.len();
    let rep = scan_now(&mut f);
    let after = events::load_journal(&f.project.dir).unwrap().events.len();
    assert_eq!(rep.created + rep.changed, 0);
    assert_eq!(before, after, "nothing was appended for an unchanged file");
}

// AT-24: content changed with mtime and size preserved is caught by the deep pass.
#[test]
fn at24_deep_verify_catches_preserved_mtime() {
    let mut f = setup("deep", &[("x.txt", b"AAAA\n")]);
    scan_initial(&mut f);
    let path = f.project_root.join("x.txt");
    // keep a copy with the original mtime outside the project (cp -p preserves it exactly)
    let ref_copy = f.base.join("x.ref");
    assert!(std::process::Command::new("cp").args(["-p"]).arg(&path).arg(&ref_copy).status().unwrap().success());
    write(&path, b"BBBB\n");
    // give the new contents exactly the old mtime, so (mtime, size) looks unchanged
    assert!(std::process::Command::new("touch")
        .arg("-r")
        .arg(&ref_copy)
        .arg(&path)
        .status()
        .unwrap()
        .success());
    let normal = scan_now(&mut f);
    assert_eq!(normal.changed, 0, "mtime and size unchanged: the normal pass sees nothing");
    let opts = ScanOptions { reason: "deep_verify".into(), deep: true, ..Default::default() };
    let deep = scan::scan_project(&f.arch, &mut f.project, &opts).unwrap();
    assert_eq!(deep.changed, 1, "the deep pass re-hashes regardless of mtime");
    let st = state_now(&f);
    assert_eq!(store::sha256_bytes(&fs::read(&path).unwrap()), st["x.txt"].hash);
}


// AT-11 / AT-12: a corrupted or missing blob is refused, never written as an empty file.
#[test]
fn at11_missing_blob_is_refused_and_reported() {
    let mut f = setup("missing", &[("a.txt", b"one\n"), ("b.txt", b"two\n")]);
    scan_initial(&mut f);
    let st = state_now(&f);
    let victim = st["a.txt"].hash.clone();
    fs::remove_file(store::blob_path(&f.project.blobs_dir(), &victim)).unwrap();
    let out = f.base.join("out");
    let rep = restore_to(&f, i64::MAX, &out);
    assert_eq!(rep.failed, 1, "the missing blob is reported, not silently skipped");
    assert!(!out.join("a.txt").exists(), "no empty file is written for the missing blob");
    assert_eq!(fs::read(out.join("b.txt")).unwrap(), b"two\n", "the healthy file is restored");
}

#[test]
fn at12_corrupted_blob_is_detected_and_quarantined() {
    let mut f = setup("corrupt", &[("a.txt", b"payload\n")]);
    scan_initial(&mut f);
    let st = state_now(&f);
    let hash = st["a.txt"].hash.clone();
    let bp = store::blob_path(&f.project.blobs_dir(), &hash);
    let mut data = fs::read(&bp).unwrap();
    data[0] ^= 0xff;
    fs::set_permissions(&bp, fs::Permissions::from(std::os::unix::fs::PermissionsExt::from_mode(0o600))).unwrap();
    fs::write(&bp, &data).unwrap();
    let store_api = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    assert!(store_api.read_verified(&hash).is_err(), "the corrupt blob must not verify");
    let out = f.base.join("out");
    let rep = restore_to(&f, i64::MAX, &out);
    assert_eq!(rep.failed, 1);
    assert!(!out.join("a.txt").exists(), "a corrupt blob never produces a file");
    // quarantine moves the bytes instead of deleting them
    store_api.quarantine(&hash, &f.project.quarantine_dir()).unwrap();
    assert!(f.project.quarantine_dir().join(&hash).is_file());
}

// AT-10: a half-written trailing journal line is ignored; corruption in the middle is an error.
#[test]
fn at10_journal_survives_a_partial_last_line() {
    let mut f = setup("journal", &[("a.txt", b"x\n")]);
    scan_initial(&mut f);
    let month = projectlife::util::month_name(projectlife::util::now_ms());
    let jf = f.project.events_dir().join(format!("{month}.jsonl"));
    let mut text = fs::read_to_string(&jf).unwrap();
    text.push_str("{\"seq\":999,\"ts\":1,\"type\":\"put\",\"path\":\"partial.txt\"");
    fs::write(&jf, text).unwrap();
    let j = events::load_journal(&f.project.dir).unwrap();
    assert!(j.trailing_partial, "the partial line is reported");
    assert!(j.events.len() >= 1, "earlier events are still read");
    // now corrupt the middle
    let mut text = fs::read_to_string(&jf).unwrap();
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if lines.len() >= 3 {
        lines[1] = "{oops".to_string();
    }
    text = lines.join("\n") + "\n";
    fs::write(&jf, text).unwrap();
    assert!(events::load_journal(&f.project.dir).is_err(), "mid-file corruption must be an error");
}

// AT-13: prune keeps the anchor state, refuses earlier moments, and frees blobs.
#[test]
fn at13_prune_keeps_anchor_state() {
    let mut f = setup("prune", &[("a.txt", b"one\n"), ("b.txt", b"two\n")]);
    scan_initial(&mut f);
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&f.project_root.join("a.txt"), b"two\n");
    scan_now(&mut f);
    let boundary = projectlife::util::now_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&f.project_root.join("a.txt"), b"three\n");
    scan_now(&mut f);
    let plan = lifecycle::prune_plan(&f.project, boundary).unwrap();
    assert_eq!(plan.anchors, 2, "both files existed at the boundary");
    let mut project = f.project.clone();
    lifecycle::prune(&f.arch, &mut project, boundary).unwrap();
    f.project = Project::from_dir(&project.dir).unwrap();
    assert_eq!(f.project.history_starts_at(), boundary);
    let out = f.base.join("after-prune");
    let rep = restore_to(&f, boundary, &out);
    assert_eq!(rep.restored, 2);
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"two\n", "the boundary state survives pruning");
    let opts = RestoreOptions {
        at: boundary - 60_000,
        at_label: "before".into(),
        paths: Vec::new(),
        to: Some(f.base.join("too-early")),
        into_project: false,
        clean: false,
        preview: false,
        missing: false,
    };
    assert!(restore::plan(&f.arch, &f.project, &opts).is_err(), "moments before the boundary are refused");
}

// AT-15: export then import reproduces the boundary states.
#[test]
fn at15_export_import_round_trip() {
    let mut f = setup("export", &[("a.txt", b"one\n"), ("dir/b.txt", b"two\n")]);
    scan_initial(&mut f);
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&f.project_root.join("a.txt"), b"changed\n");
    scan_now(&mut f);
    let out = f.base.join("export");
    let rep = lifecycle::export(&f.arch, &f.project, None, None, &out).unwrap();
    assert!(rep.verified, "every exported blob must re-verify");
    let (ok, bad) = lifecycle::verify_manifest(&out).unwrap();
    assert!(bad.is_empty(), "manifest mismatches: {bad:?}");
    assert!(ok >= rep.blobs + 1, "manifest covers blobs plus the journal");
    let imp = lifecycle::import(&f.arch, &out, None, Some("copy")).unwrap();
    let imported = f.arch.find("copy").unwrap();
    let j = events::load_journal(&imported.dir).unwrap();
    let a_state = events::state_at(&j.events, i64::MAX, None);
    assert_eq!(a_state.len(), 2);
    assert_eq!(a_state["a.txt"].hash, state_now(&f)["a.txt"].hash);
    assert!(imp.events_added > 0);
}

// AT-26 / AT-27: mass deletion raises a mass event and last-good restores the earlier state.
#[test]
fn at26_mass_delete_and_last_good() {
    let files: Vec<(String, Vec<u8>)> = (0..8)
        .map(|i| (format!("f{i}.txt"), format!("file {i}\n").into_bytes()))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let mut f = setup("mass", &borrowed);
    scan_initial(&mut f);
    std::thread::sleep(std::time::Duration::from_millis(5));
    for (n, _) in &files {
        fs::remove_file(f.project_root.join(n)).unwrap();
    }
    let rep = scan_now(&mut f);
    let mass = rep.mass.expect("a mass event must be raised");
    assert_eq!(events::get_str(&mass, "kind").as_deref(), Some("mass_delete"));
    let j = events::load_journal(&f.project.dir).unwrap();
    let (good_ts, why) = restore::last_good(&j.events, &f.project).expect("last good state");
    assert!(why.contains("mass_delete"));
    let out = f.base.join("recovered");
    let r = restore_to(&f, good_ts, &out);
    assert_eq!(r.restored, 8, "every file comes back");
    for (n, d) in &files {
        assert_eq!(&fs::read(out.join(n)).unwrap(), d);
    }
}

// AT-21: unicode, spaces, CRLF, BOM and empty files survive byte for byte.
#[test]
fn at21_unicode_crlf_bom_empty_round_trip() {
    let mut f = setup(
        "unicode",
        &[
            ("dir with space/файл-имя.txt", b"content\n"),
            ("crlf.txt", b"line1\r\nline2\r\n"),
            ("bom.txt", b"\xef\xbb\xbfwith bom\n"),
            ("empty.txt", b""),
            ("emoji-😀.txt", b"emoji name\n"),
        ],
    );
    scan_initial(&mut f);
    let out = f.base.join("out");
    let rep = restore_to(&f, i64::MAX, &out);
    assert_eq!(rep.restored, 5);
    for rel in [
        "dir with space/файл-имя.txt",
        "crlf.txt",
        "bom.txt",
        "empty.txt",
        "emoji-\u{1f600}.txt",
    ] {
        assert_eq!(
            fs::read(f.project_root.join(rel)).unwrap(),
            fs::read(out.join(rel)).unwrap(),
            "{rel} must round-trip byte for byte"
        );
    }
    assert!(out.join("empty.txt").is_file(), "an empty file is a full version");
}

// AT-29: filters explain themselves — secrets, dependencies, binaries, hidden files.
#[test]
fn at29_filters_report_reasons() {
    let mut f = setup(
        "filters",
        &[
            ("src/app.ts", b"ok\n"),
            (".env", b"SECRET=1\n"),
            ("node_modules/pkg/index.js", b"dep\n"),
            ("big.png", b"\x89PNG binary"),
            (".hidden", b"hidden\n"),
            ("huge.txt", &vec![b'x'; 4096]),
        ],
    );
    // tighten the size limit so the last file is skipped for a reason we can assert
    let mut settings = serde_json::Map::new();
    settings.insert("maxFileSizeKb".into(), serde_json::Value::from(1));
    settings.insert("maxFileSizeKb".into(), serde_json::Value::from(1));
    f.project.settings_mut().insert("maxFileSizeKb".into(), serde_json::Value::from(1));
    f.project.save_meta().unwrap();
    let rep = scan_initial(&mut f);
    let reasons: BTreeMap<String, usize> = rep.skipped_by_reason.clone();
    let looked: BTreeMap<&String, &usize> = reasons.iter().collect();
    let has = |r: &str| looked.keys().any(|k| k.as_str() == r);
    assert!(has("secret"), "secrets are skipped with a reason: {reasons:?}");
    assert!(has("ignored_dir"), "dependencies are skipped: {reasons:?}");
    assert!(has("binary"), "binaries are skipped in the source profile: {reasons:?}");
    assert!(has("hidden"), "hidden files are skipped: {reasons:?}");
    assert!(has("too_large"), "oversized files are skipped: {reasons:?}");
    let st = state_now(&f);
    assert!(st.contains_key("src/app.ts"));
    assert!(!st.contains_key(".env"));
    assert!(!st.contains_key("huge.txt"));
    // the filter decision API reports the rule, which is what `pl why` prints
    let filter = FilterConfig::for_profile("source", &f.project.settings());
    let own = projectlife::glob::IgnoreRules::empty();
    let d = filter.decide_file(".env", 0, &own, &own);
    assert!(!d.track);
    assert_eq!(d.reason.as_deref(), Some("secret"));
}

// AT-25: one writer at a time.
#[test]
fn at25_second_writer_is_refused() {
    let f = setup("lock", &[("a.txt", b"x\n")]);
    let lock = f.arch.lock("test").unwrap();
    assert!(f.arch.lock("other").is_err(), "the second writer must be refused");
    lock.release();
    assert!(f.arch.lock("third").is_ok());
}

// Symlinks are recorded, never dereferenced, and an outside target is not recreated.
#[test]
fn symlinks_are_recorded_not_dereferenced() {
    let mut f = setup("symlink", &[("inside.txt", b"data\n")]);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("inside.txt", f.project_root.join("link_inside")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", f.project_root.join("link_outside")).unwrap();
    }
    let rep = scan_initial(&mut f);
    assert!(rep.files_on_disk >= 1);
    let j = events::load_journal(&f.project.dir).unwrap();
    let symlinks: Vec<&events::Ev> = j
        .events
        .iter()
        .filter(|e| events::event_type(e) == "symlink")
        .collect();
    assert_eq!(symlinks.len(), 2, "both symlinks are recorded as events");
    let st = state_now(&f);
    assert_eq!(st["link_inside"].kind, "symlink");
    let out = f.base.join("out");
    let r = restore_to(&f, i64::MAX, &out);
    assert!(out.join("link_inside").exists(), "an inside symlink is recreated");
    assert!(!out.join("link_outside").exists(), "an outside symlink is not created");
    assert!(r.skipped_symlinks >= 1 || !r.warnings.is_empty());
    // the dereferenced content of /etc/passwd must never appear in the archive
    let store_api = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    for (h, _) in store_api.list_all() {
        let data = store_api.read_verified(&h).unwrap();
        assert!(!data.starts_with(b"root:"), "symlink targets are never copied");
    }
}

// Path traversal: a journal entry that escapes the target is refused.
#[test]
fn traversal_paths_are_refused() {
    let mut f = setup("traversal", &[("ok.txt", b"fine\n")]);
    scan_initial(&mut f);
    // forge an event pointing outside the target
    let mut ev = events::ev_new(0, projectlife::util::now_ms(), "put");
    events::put_str(&mut ev, "path", "../escaped.txt");
    events::put_str(&mut ev, "hash", &store::sha256_bytes(b"escape\n"));
    events::put_u64(&mut ev, "size", 7);
    let mut v = vec![ev];
    events::append(&f.project, &mut v).unwrap();
    let store_api = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    store_api.put(&store::sha256_bytes(b"escape\n"), b"escape\n").unwrap();
    let out = f.base.join("out");
    let opts = RestoreOptions {
        at: i64::MAX,
        at_label: "test".into(),
        paths: Vec::new(),
        to: Some(out.clone()),
        into_project: false,
        clean: false,
        preview: false,
        missing: false,
    };
    let plan = restore::plan(&f.arch, &f.project, &opts).unwrap();
    assert!(plan.warnings.iter().any(|w| w.contains("escapes the target")));
    restore::execute(&f.project, &plan, &opts).unwrap();
    assert!(!f.base.join("escaped.txt").exists(), "nothing may be written outside the target");
    assert!(out.join("ok.txt").is_file());
}

// A project whose folder disappeared is reported as path_missing and keeps its history.
#[test]
fn missing_project_path_keeps_history() {
    let mut f = setup("vanished", &[("a.txt", b"data\n")]);
    scan_initial(&mut f);
    fs::remove_dir_all(&f.project_root).unwrap();
    let err = scan::scan_project(
        &f.arch,
        &mut f.project,
        &ScanOptions { reason: "observed".into(), ..Default::default() },
    )
    .unwrap_err();
    assert!(err.contains("path_missing"), "unexpected error: {err}");
    let reloaded = Project::from_dir(&f.project.dir).unwrap();
    assert_eq!(reloaded.state(), "path_missing");
    let out = f.base.join("out");
    let rep = restore_to(&f, i64::MAX, &out);
    assert_eq!(rep.restored, 1, "history is untouched and still restorable");
}

// A mass rewrite is reported as suspicious_rewrite rather than drowning in ordinary puts.
#[test]
fn suspicious_rewrite_is_its_own_kind() {
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("s{i}.txt"), format!("old {i}\n").into_bytes()))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let mut f = setup("rewrite", &borrowed);
    scan_initial(&mut f);
    std::thread::sleep(std::time::Duration::from_millis(5));
    for i in 0..6 {
        write(&f.project_root.join(format!("s{i}.txt")), format!("new {i}\n").as_bytes());
    }
    // lower the mass thresholds so the rewrite case is what triggers, not the "big change" case
    let mut cfg = f.arch.config.clone();
    cfg.set("massChangeFiles", serde_json::Value::from(1000));
    cfg.set("massChangePercent", serde_json::Value::from(99));
    cfg.save(&f.arch.root).unwrap();
    f.arch.config = cfg;
    let rep = scan_now(&mut f);
    let mass = rep.mass.expect("a mass event is raised for a full rewrite");
    assert_eq!(events::get_str(&mass, "kind").as_deref(), Some("suspicious_rewrite"));
}

// ---------------------------------------------------------------------------------------------
// Round 289. Every test below states the defect it is meant to catch; each one is written so that
// it fails if the fix is removed (the mutation campaign in tools/verify.sh checks exactly that).
// ---------------------------------------------------------------------------------------------

fn blob_hashes(f: &Fixture) -> Vec<String> {
    Store::new(&f.project.blobs_dir(), &f.project.tmp_dir())
        .list_all()
        .into_iter()
        .map(|(h, _)| h)
        .collect()
}

// A. The reader itself refuses to look through a symlink, not only the caller. Without the
// O_NOFOLLOW open this call returns the target's bytes.
#[test]
fn read_stable_refuses_to_follow_a_symlink() {
    let f = setup("nofollow", &[("real.txt", b"real bytes\n")]);
    #[cfg(unix)]
    std::os::unix::fs::symlink(f.project_root.join("real.txt"), f.project_root.join("link")).unwrap();
    let err = scan::read_stable(&f.project_root.join("link")).unwrap_err();
    assert!(err.contains("symlink"), "expected a refusal, got: {err}");
    // positive control: the same call on the real path still reads the bytes
    assert_eq!(scan::read_stable(&f.project_root.join("real.txt")).unwrap(), b"real bytes\n");
}

// A.2 A symlink to a secret OUTSIDE the project produces no blob at all — not "a blob that is
// later deleted", and not under another name.
#[test]
fn symlink_to_secret_outside_never_becomes_a_blob() {
    let mut f = setup("secretlink", &[("inside.txt", b"inside\n")]);
    let secret = f.base.join("outside-secret.txt");
    fs::write(&secret, b"TOP-SECRET-CONTENT-9001\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, f.project_root.join("link_to_secret")).unwrap();
    let rep = scan_initial(&mut f);
    assert!(rep.files_on_disk >= 1);
    // A link is not an unreadable file: if removing the loop's symlink guard made the reader try
    // and fail on it, the cycle would report `unstable` here. That is the difference between a
    // recorded link and a file whose bytes nobody could get.
    assert_eq!(rep.unstable, 0, "a symlink is recorded as a link, not as an unreadable file");

    // 1. the path is tracked as a link, with no content hash
    let st = state_now(&f);
    assert_eq!(st["link_to_secret"].kind, "symlink");
    assert!(st["link_to_secret"].hash.is_empty(), "a link must never carry a content hash");

    // 2. no `put` event for it — only a `symlink` event carrying the target path
    let j = events::load_journal(&f.project.dir).unwrap();
    assert!(
        !j.events.iter().any(|e| {
            events::event_type(e) == "put" && events::get_str(e, "path").as_deref() == Some("link_to_secret")
        }),
        "a symlink must not be stored as a version of itself"
    );
    assert_eq!(
        j.events.iter().filter(|e| events::event_type(e) == "symlink").count(),
        1,
        "the link is recorded exactly once, as a link"
    );

    // 3. the target's bytes are in no blob, and the store holds only the ordinary file
    let store = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    let all = store.list_all();
    assert_eq!(all.len(), 1, "only the ordinary file may have a blob: {all:?}");
    for (h, _) in &all {
        let data = store.read_verified(h).unwrap();
        assert!(
            !String::from_utf8_lossy(&data).contains("TOP-SECRET-CONTENT-9001"),
            "blob {h} holds the symlink's target"
        );
    }
}

// A.3 A symlink pointing INTO the archive is skipped with a stated reason and stores nothing —
// including the archive's own config.json.
#[test]
fn symlink_into_the_archive_is_skipped_not_stored() {
    let mut f = setup("arclink", &[("inside.txt", b"inside\n")]);
    #[cfg(unix)]
    std::os::unix::fs::symlink(f.arch.root.join("config.json"), f.project_root.join("link_to_archive"))
        .unwrap();
    let rep = scan_initial(&mut f);
    assert_eq!(rep.unstable, 0, "a skipped archive symlink is never read at all");
    assert_eq!(
        rep.skipped_by_reason.get("symlink_to_archive").copied().unwrap_or(0),
        1,
        "the archive symlink must be skipped with its own reason: {:?}",
        rep.skipped_by_reason
    );
    let j = events::load_journal(&f.project.dir).unwrap();
    assert!(j.events.iter().any(|e| {
        events::event_type(e) == "skip"
            && events::get_str(e, "path").as_deref() == Some("link_to_archive")
            && events::get_str(e, "reason").as_deref() == Some("symlink_to_archive")
    }));
    assert!(
        !j.events.iter().any(|e| {
            events::get_str(e, "path").as_deref() == Some("link_to_archive")
                && matches!(events::event_type(e), "put" | "symlink")
        }),
        "a skipped archive symlink produces no version event"
    );
    let store = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    for (h, _) in store.list_all() {
        let data = store.read_verified(&h).unwrap();
        assert!(
            !String::from_utf8_lossy(&data).contains("intervalSeconds"),
            "blob {h} holds the archive's own config"
        );
    }
}

// B.1 A rename that does not change the contents: one move, no new blob, the old path is not
// reported as deleted, and the new path inherits the history.
#[test]
fn rename_without_content_change_is_one_move_and_no_new_blob() {
    let mut f = setup("mv1", &[("a.txt", b"alpha\n")]);
    scan_initial(&mut f);
    let blobs_before = blob_hashes(&f);
    fs::rename(f.project_root.join("a.txt"), f.project_root.join("b.txt")).unwrap();
    let rep = scan_now(&mut f);
    let j = events::load_journal(&f.project.dir).unwrap();
    let moves: Vec<&events::Ev> = j.events.iter().filter(|e| events::event_type(e) == "move").collect();
    assert_eq!(moves.len(), 1, "a rename is one move event");
    assert_eq!(events::get_str(moves[0], "from").as_deref(), Some("a.txt"));
    assert_eq!(events::get_str(moves[0], "to").as_deref(), Some("b.txt"));
    assert_eq!(rep.moved, 1);
    assert_eq!(rep.blobs_new, 0, "an unchanged rename stores no new blob");
    assert_eq!(rep.deleted, 0, "the old path must not be reported as deleted");
    assert_eq!(blob_hashes(&f), blobs_before, "the blob store is unchanged");
    let st = state_now(&f);
    assert!(!st.contains_key("a.txt"));
    assert_eq!(st["b.txt"].hash, store::sha256_bytes(b"alpha\n"), "history followed the rename");
    // the recorded version is the bytes that are on disk under the new name
    assert_eq!(st["b.txt"].hash, hash_of(&f.project_root.join("b.txt")));
}

// B.2 A rename with a content change: the move comes first, the put follows, and the new version
// is the bytes on disk.
#[test]
fn rename_with_content_change_is_move_then_put() {
    let mut f = setup("mv2", &[("a.txt", b"alpha\n")]);
    scan_initial(&mut f);
    fs::rename(f.project_root.join("a.txt"), f.project_root.join("b.txt")).unwrap();
    write(&f.project_root.join("b.txt"), b"alpha CHANGED\n");
    let rep = scan_now(&mut f);
    let j = events::load_journal(&f.project.dir).unwrap();
    let moves: Vec<&events::Ev> = j.events.iter().filter(|e| events::event_type(e) == "move").collect();
    let puts: Vec<&events::Ev> = j
        .events
        .iter()
        .filter(|e| events::event_type(e) == "put" && events::get_str(e, "path").as_deref() == Some("b.txt"))
        .collect();
    assert_eq!(moves.len(), 1);
    assert_eq!(puts.len(), 1, "the changed contents need their own version");
    assert!(
        events::seq_of(moves[0]) < events::seq_of(puts[0]),
        "the move must be written before the put that follows it (FR-WCH-9)"
    );
    assert!(rep.blobs_new >= 1);
    assert_eq!(rep.deleted, 0);
    let st = state_now(&f);
    assert_eq!(st["b.txt"].hash, store::sha256_bytes(b"alpha CHANGED\n"));
}

// B.3 A folder rename is one move per file inside it, all in one batch.
#[test]
fn folder_rename_is_a_batch_of_moves() {
    let mut f = setup("mvdir", &[("sub/a.txt", b"a\n"), ("sub/b.txt", b"b\n"), ("sub/c.txt", b"c\n")]);
    scan_initial(&mut f);
    fs::rename(f.project_root.join("sub"), f.project_root.join("sub2")).unwrap();
    let rep = scan_now(&mut f);
    let j = events::load_journal(&f.project.dir).unwrap();
    let moves: Vec<&events::Ev> = j.events.iter().filter(|e| events::event_type(e) == "move").collect();
    assert_eq!(moves.len(), 3, "one move per file in the renamed folder");
    let batches: std::collections::BTreeSet<String> =
        moves.iter().filter_map(|e| events::get_str(e, "batchId")).collect();
    assert_eq!(batches.len(), 1, "one cycle writes one batch id");
    for m in &moves {
        assert!(events::get_str(m, "from").unwrap().starts_with("sub/"), "{m:?}");
        assert!(events::get_str(m, "to").unwrap().starts_with("sub2/"), "{m:?}");
    }
    assert_eq!(rep.deleted, 0, "no file of the renamed folder is reported deleted");
    assert_eq!(rep.moved, 3);
}

// G. The observed window is kept in chronological order and truncated by POSITION. The bug this
// catches: sorting the list before truncating drops the smallest intervals and keeps the old ones
// forever, so the reported window stops describing the recent past.
#[test]
fn observed_window_is_truncated_by_age_not_by_value() {
    let mut f = setup("window", &[("a.txt", b"x\n")]);
    scan_initial(&mut f);
    // 50 old large intervals followed by 150 small ones, in chronological order
    let mut seed: Vec<serde_json::Value> = vec![serde_json::Value::from(900_000i64); 50];
    seed.extend(vec![serde_json::Value::from(5i64); 150]);
    f.project.set_meta("observedIntervalsMs", serde_json::Value::Array(seed));
    f.project.save_meta().unwrap();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        scan_now(&mut f);
    }
    let reloaded = Project::from_dir(&f.project.dir).unwrap();
    let arr: Vec<i64> = reloaded
        .meta
        .get("observedIntervalsMs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    assert_eq!(arr.len(), 200, "the window stays bounded");
    assert!(
        !arr.contains(&900_000),
        "the 50 oldest samples are the ones that must leave, whatever their value"
    );
    let med = reloaded.meta.get("observedIntervalMs").and_then(|v| v.get("median")).and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(med > 0 && med < 1_000, "the reported median must come from the kept samples, got {med}");
}

fn run_bin(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", home);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.args(args);
    c.output().expect("the shipped binary must be runnable")
}

/// The state that matters: identity unaffected by sequence renumbering.
fn state_sig(f: &Fixture) -> BTreeMap<String, (String, u64, u32, String, Option<String>)> {
    state_now(f)
        .into_iter()
        .map(|(k, v)| (k, (v.hash, v.size, v.mode, v.kind, v.target)))
        .collect()
}

/// No referenced blob is missing and no unreferenced blob is left behind.
fn archive_consistent(f: &Fixture) -> Result<(), String> {
    let j = events::load_journal(&f.project.dir)?;
    let store = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in &j.events {
        if events::event_type(e) == "put" {
            if let Some(h) = events::get_str(e, "hash") {
                referenced.insert(h);
            }
        }
    }
    let missing: Vec<String> = referenced.iter().filter(|h| !store.has(h)).cloned().collect();
    if !missing.is_empty() {
        return Err(format!("blobs referenced but absent: {missing:?}"));
    }
    let dangling: Vec<String> = store
        .list_all()
        .into_iter()
        .map(|(h, _)| h)
        .filter(|h| !referenced.contains(h))
        .collect();
    if !dangling.is_empty() {
        return Err(format!("blobs present but unreferenced: {dangling:?}"));
    }
    Ok(())
}

/// A project with three versions of the same file and a boundary moment between two of them.
fn prune_fixture(name: &str) -> (Fixture, i64) {
    let mut f = setup(name, &[("a.txt", b"one\n"), ("b.txt", b"two\n")]);
    scan_initial(&mut f);
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&f.project_root.join("a.txt"), b"three\n");
    scan_now(&mut f);
    let boundary = projectlife::util::now_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&f.project_root.join("a.txt"), b"four\n");
    scan_now(&mut f);
    (f, boundary)
}

/// C. Kill -9 in the middle of a prune, then recover through the ordinary path.
///
/// The invariant: whichever phase the crash lands in, the next run leaves a readable journal, a
/// consistent archive (no missing and no dangling blobs), and the SAME current state as before the
/// prune started. Nothing here signs off on "it should be fine" — the state signature is compared.
fn prune_crash_and_recover(phase: &str, expect_no_events_dir: bool, expect_completed: bool) {
    let name = "prunecrash";
    let (f, boundary) = prune_fixture(name);
    let before = state_sig(&f);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let out = run_bin(
        &home,
        &["--archive", &archive, "prune", name, "--before", &boundary.to_string(), "--yes"],
        &[("PROJECTLIFE_CRASH_AFTER", phase)],
    );
    assert!(
        out.status.code().is_none(),
        "phase {phase}: the process must have been killed by a signal, not have returned: code {:?}\n{}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.project.prune_journal().is_file(),
        "phase {phase}: the phase file must be on disk after the crash"
    );
    if expect_no_events_dir {
        assert!(
            !f.project.events_dir().is_dir(),
            "phase {phase}: between the two renames the project has no events directory"
        );
    }

    // The ordinary path heals it: `scan-once` recovers before it observes.
    let rec = run_bin(&home, &["--archive", &archive, "scan-once", name], &[]);
    assert!(
        rec.status.success(),
        "phase {phase}: recovery failed: {}\n{}",
        String::from_utf8_lossy(&rec.stdout),
        String::from_utf8_lossy(&rec.stderr)
    );
    assert!(!f.project.prune_journal().is_file(), "phase {phase}: the phase file must be gone");

    let j = events::load_journal(&f.project.dir).expect("the journal must be readable after recovery");
    assert!(!j.events.is_empty(), "phase {phase}: recovery must leave a usable history");
    assert_eq!(
        state_sig(&f),
        before,
        "phase {phase}: an interrupted-and-recovered prune must not change the current state"
    );
    assert_eq!(archive_consistent(&f).err(), None, "phase {phase}");

    // Which branch ran is not cosmetic: a rolled-back prune must leave historyStartsAt where it
    // was, a completed one must move it to the boundary. Asserting the state alone would accept
    // both, and would not notice a recovery that kept the new journal but never corrected it.
    let reloaded = Project::from_dir(&f.project.dir).unwrap();
    if expect_completed {
        assert_eq!(
            reloaded.history_starts_at(),
            boundary,
            "phase {phase}: a completed recovery must record the new start of history"
        );
    } else {
        assert!(
            reloaded.history_starts_at() < boundary,
            "phase {phase}: a rolled-back prune must leave the start of history untouched"
        );
    }

    // and the boundary moment is still restorable, with the contents it had
    let out_dir = f.base.join(format!("restore-{phase}"));
    restore_to(&f, boundary, &out_dir);
    assert_eq!(fs::read(out_dir.join("a.txt")).unwrap(), b"three\n", "phase {phase}");
    assert_eq!(fs::read(out_dir.join("b.txt")).unwrap(), b"two\n", "phase {phase}");
}

#[test]
fn prune_killed_at_the_very_start_rolls_back() {
    prune_crash_and_recover("start", false, false);
}

#[test]
fn prune_killed_after_the_first_rename_rolls_back() {
    prune_crash_and_recover("first_rename", true, false);
}

#[test]
fn prune_killed_after_the_journal_swap_completes() {
    prune_crash_and_recover("journal_swapped", false, true);
}

#[test]
fn prune_killed_after_the_metadata_save_completes() {
    prune_crash_and_recover("meta_saved", false, true);
}

#[test]
fn prune_killed_while_deleting_blobs_completes() {
    prune_crash_and_recover("blobs_deleting", false, true);
}

// D. Importing history into a project that already has a journal is refused, and the refusal is
// not superstition: the calibration below performs exactly the refused append by hand and shows
// that it really does resurrect a file deleted after the export range.
#[test]
fn import_into_an_existing_project_is_refused_and_changes_nothing() {
    // source: two files, then one of them changes, and the export covers everything
    let mut src = setup("impsrc", &[("gone.txt", b"old\n"), ("kept.txt", b"keep\n")]);
    scan_initial(&mut src);
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(&src.project_root.join("kept.txt"), b"keep2\n");
    scan_now(&mut src);
    let export_dir = src.base.join("export");
    let rep = lifecycle::export(&src.arch, &src.project, None, None, &export_dir).unwrap();
    assert!(rep.verified);

    // target: the same file names, but gone.txt was deleted AFTER the export range
    let mut tgt = setup("imptgt", &[("gone.txt", b"old\n"), ("kept.txt", b"keep\n")]);
    scan_initial(&mut tgt);
    std::thread::sleep(std::time::Duration::from_millis(5));
    fs::remove_file(tgt.project_root.join("gone.txt")).unwrap();
    scan_now(&mut tgt);
    let before = state_sig(&tgt);
    assert!(!before.contains_key("gone.txt"), "the deleted file must be gone from the state");

    let err = lifecycle::import(&tgt.arch, &export_dir, Some("imptgt"), None).unwrap_err();
    assert!(err.contains("refused"), "expected a refusal, got: {err}");
    assert!(err.contains("--new"), "the refusal must name the safe alternative: {err}");
    assert_eq!(state_sig(&tgt), before, "a refused import must not touch the current state");

    // the safe shape still works: the same export into its own project
    let ok = lifecycle::import(&tgt.arch, &export_dir, None, Some("imported")).unwrap();
    assert!(ok.events_added > 0);

    // calibration: append the very events the refusal prevented, and watch the state break
    let imported_journal = events::load_journal(&export_dir).unwrap();
    let mut by_hand: Vec<events::Ev> = imported_journal.events.clone();
    for e in by_hand.iter_mut() {
        e.insert("imported".into(), serde_json::Value::from(true));
    }
    events::append(&tgt.project, &mut by_hand).unwrap();
    let after = state_sig(&tgt);
    assert!(
        after.contains_key("gone.txt"),
        "calibration: the append this refusal prevents must really resurrect the deleted file — \
         otherwise the refusal is protecting against nothing"
    );
}

// E.1 A lock whose owner is dead is taken over by the next cycle instead of blocking forever.
#[test]
fn dead_holder_lock_is_taken_over() {
    let f = setup("lockdead", &[("a.txt", b"x\n")]);
    // a pid that is certainly gone: a child that has already been reaped
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    child.wait().unwrap();
    let lock = f.arch.root.join(".lock");
    fs::write(&lock, format!("cycle {pid} {}\n", projectlife::util::now_ms())).unwrap();
    let got = f.arch.lock("cycle").expect("a lock left by a dead process must be taken over");
    got.release();
    let log = fs::read_to_string(f.arch.root.join("logs/projectlife.log")).unwrap_or_default();
    assert!(log.contains("took over a stale lock"), "the takeover must be in the log: {log}");
}

// E.2 The negative control: a lock held by a LIVE process is never displaced.
#[test]
fn live_holder_lock_is_not_stolen() {
    let f = setup("locklive", &[("a.txt", b"x\n")]);
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id() as i32;
    let lock = f.arch.root.join(".lock");
    fs::write(&lock, format!("cycle {pid} {}\n", projectlife::util::now_ms())).unwrap();
    let err = f.arch.lock("cycle").err().expect("a running holder must not be displaced");
    assert!(err.contains("already in use"), "{err}");
    assert!(lock.is_file(), "the live holder's lock file must still be there");
    let _ = child.kill();
    let _ = child.wait();
}

// E.3 One command removes a lock whose owner is gone — and refuses to remove a live one.
#[test]
fn doctor_fix_lock_removes_a_dead_lock_only() {
    let f = setup("lockfix", &[("a.txt", b"x\n")]);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let lock = f.arch.root.join(".lock");
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id() as i32;
    child.wait().unwrap();
    fs::write(&lock, format!("cycle {dead} {}\n", projectlife::util::now_ms())).unwrap();
    let out = run_bin(&home, &["--archive", &archive, "doctor", "--fix-lock"], &[]);
    assert!(
        out.status.success(),
        "--fix-lock must succeed on a dead lock: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!lock.exists(), "the stale lock must be gone");

    let mut live = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    fs::write(&lock, format!("cycle {} {}\n", live.id(), projectlife::util::now_ms())).unwrap();
    let out = run_bin(&home, &["--archive", &archive, "doctor", "--fix-lock"], &[]);
    assert!(!out.status.success(), "--fix-lock must refuse to remove a live holder's lock");
    assert!(lock.is_file(), "the live holder's lock must survive");
    let _ = live.kill();
    let _ = live.wait();
}

// F.1 The ordinary cycle does not count (does not open) a skipped directory. The positive control
// is the same cycle with --skipped: if that were not slower, this test could not tell the two
// modes apart and would prove nothing.
#[test]
fn ordinary_cycle_does_not_count_a_skipped_directory() {
    let mut f = setup("deps", &[("src/app.ts", b"ok\n")]);
    let nm = f.project_root.join("node_modules/pkg");
    fs::create_dir_all(&nm).unwrap();
    for i in 0..50_000 {
        let _ = fs::File::create(nm.join(format!("dep{i:06}.js")));
    }
    scan_initial(&mut f);
    let mut ordinary = Vec::new();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        scan_now(&mut f);
        ordinary.push(t.elapsed());
    }
    ordinary.sort();
    let median = ordinary[1];
    let rep = scan_now(&mut f);
    let nm_rule = rep
        .skipped_paths
        .iter()
        .find(|(p, _, _)| p == "node_modules")
        .map(|(_, r, rule)| (r.clone(), rule.clone()))
        .expect("node_modules must be reported as skipped");
    assert_eq!(nm_rule.0, "ignored_dir");
    assert!(
        !nm_rule.1.contains(" files)"),
        "the ordinary cycle must record the skip without counting it: {nm_rule:?}"
    );
    assert!(
        median.as_millis() < 1000,
        "NFR-PRF-2: an ordinary cycle must stay under a second, got {median:?}"
    );

    let opts = ScanOptions { reason: "observed".into(), count_skipped: true, ..Default::default() };
    let t = std::time::Instant::now();
    let counted = scan::scan_project(&f.arch, &mut f.project, &opts).unwrap();
    let counted_dt = t.elapsed();
    let counted_rule = counted
        .skipped_paths
        .iter()
        .find(|(p, _, _)| p == "node_modules")
        .map(|(_, _, rule)| rule.clone())
        .unwrap_or_default();
    assert!(counted_rule.contains("50000 files"), "--skipped must count: {counted_rule}");
    assert!(
        counted_dt > median * 3,
        "positive control: counting 50 000 entries must be measurably slower than not counting \
         (counted {counted_dt:?} vs ordinary {median:?}) — otherwise this test proves nothing"
    );
}

// F.2 The NFR-PRF-2 promise itself, on the shipped promise wording: 10 000 unchanged files.
#[test]
fn ordinary_cycle_over_10000_unchanged_files_is_under_a_second() {
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        files.push((format!("src{:03}/file{:05}.txt", i / 100, i), format!("content {i}\n").into_bytes()));
    }
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let mut f = setup("files10k", &borrowed);
    scan_initial(&mut f);
    let mut runs = Vec::new();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let rep = scan_now(&mut f);
        runs.push(t.elapsed());
        assert_eq!(rep.changed + rep.created + rep.deleted, 0, "the files are untouched");
        assert_eq!(rep.files_on_disk, 10_000);
    }
    runs.sort();
    assert!(
        runs[1].as_millis() < 1000,
        "NFR-PRF-2: 10 000 unchanged files in under a second, got {:?} (runs {runs:?})",
        runs[1]
    );
}

// ---------------------------------------------------------------------------------------------
// Round 290. The filesystem-notification trigger.
//
// Every test here runs a REAL daemon process against a REAL filesystem and then reads the archive,
// the log and the trigger state the daemon wrote. The requirement each one proves is named in its
// own comment; whenever a test asserts that something did NOT happen, it also asserts that the
// measurement itself is alive (a positive control), because a watcher that watches nothing would
// otherwise pass the negative half.
// ---------------------------------------------------------------------------------------------

/// Start a real daemon over this fixture. `envs` carries the deliberate fault injectors.
fn spawn_daemon(f: &Fixture, envs: &[(&str, &str)], no_watch: bool) -> std::process::Child {
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", &home);
    c.arg("--archive").arg(f.arch.root.to_string_lossy().to_string()).arg("daemon").arg("run");
    if no_watch {
        c.arg("--no-watch");
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c.spawn().expect("the shipped binary must be runnable")
}

/// Ask the daemon to stop the polite way (SIGTERM), then reap it.
fn stop_daemon(child: &mut std::process::Child) {
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status();
    for _ in 0..100 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn watch_state(f: &Fixture) -> serde_json::Value {
    match fs::read_to_string(f.arch.root.join("watch_state.json")) {
        Ok(t) => serde_json::from_str(&t).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

fn st_u64(v: &serde_json::Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Wait until the daemon with this pid has installed its watch set AND completed its first pass.
///
/// The pid matters: `watch_state.json` survives a daemon, so waiting for "a state file with a watch
/// set" would be satisfied by the PREVIOUS daemon's file and the test would assert against a process
/// that has not started yet.
///
/// The first pass matters too: the state file is written at the end of a pass, so `periodicCycles >= 1`
/// proves the startup pass is over. A change made before that could be picked up by the startup pass and
/// the test would then call a periodic result "triggered".
fn wait_for_watches(f: &Fixture, pid: u32, timeout_ms: i64) -> serde_json::Value {
    let started = projectlife::util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && st_u64(&st, "watchedDirs") > 0 && st_u64(&st, "periodicCycles") >= 1 {
            return st;
        }
        if projectlife::util::now_ms() - started > timeout_ms {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The same, for a daemon started with `--no-watch` (no watch set to wait for).
fn wait_for_daemon_first_pass(f: &Fixture, pid: u32, timeout_ms: i64) -> serde_json::Value {
    let started = projectlife::util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && st_u64(&st, "periodicCycles") >= 1 {
            return st;
        }
        if projectlife::util::now_ms() - started > timeout_ms {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Wait until the daemon has recorded at least `at_least` trigger-driven passes.
///
/// The state file is written at the END of a pass, while the journal is written inside it, so reading
/// the state the instant a version appears can catch the file one pass too early. That is a race in
/// the reading, not in the daemon — this helper removes it.
fn wait_for_trigger_count(f: &Fixture, pid: u32, at_least: u64, timeout_ms: i64) -> serde_json::Value {
    let started = projectlife::util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && st_u64(&st, "triggerCycles") >= at_least {
            return st;
        }
        if projectlife::util::now_ms() - started > timeout_ms {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Wait for the daemon to publish a state that satisfies `pred`.
///
/// The state file is written at the END of a pass and the journal inside it, so a version can be on
/// disk while the published counters still describe the previous pass. Tests that assert on counters
/// wait for the state itself instead of reading whatever happens to be there.
fn wait_for_state(f: &Fixture, pid: u32, timeout_ms: i64, what: &str, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let started = projectlife::util::now_ms();
    loop {
        let st = watch_state(f);
        if st_u64(&st, "pid") == pid as u64 && pred(&st) {
            return st;
        }
        if projectlife::util::now_ms() - started > timeout_ms {
            eprintln!("wait_for_state: {what} not reached within {timeout_ms} ms; last state: {st}");
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The interval this test asks for, with the adaptive growth switched off: the point of the test is
/// the configured interval, not what a loaded machine turns it into.
fn set_interval(f: &Fixture, secs: i64) {
    set_config(f, "intervalSeconds", serde_json::Value::from(secs));
    set_config(f, "autoInterval", serde_json::Value::from(false));
}

fn set_config(f: &Fixture, key: &str, v: serde_json::Value) {    let mut cfg = Archive::open(&f.arch.root).unwrap().config;
    cfg.set(key, v);
    cfg.save(&f.arch.root).unwrap();
}

/// Every `put` in the journal as (path, hash).
fn journal_puts(f: &Fixture) -> Vec<(String, String)> {
    let j = events::load_journal(&f.project.dir).unwrap_or(events::Journal { events: Vec::new(), trailing_partial: false });
    j.events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .map(|e| (events::get_str(e, "path").unwrap_or_default(), events::get_str(e, "hash").unwrap_or_default()))
        .collect()
}

/// Wait until a `put` holds the bytes that are on disk right now. Returns the latency in ms.
fn wait_for_current_bytes(f: &Fixture, rel: &str, timeout_ms: i64) -> Option<i64> {
    let want = hash_of(&f.project_root.join(rel));
    let started = projectlife::util::now_ms();
    loop {
        if journal_puts(f).iter().any(|(p, h)| p == rel && *h == want) {
            return Some(projectlife::util::now_ms() - started);
        }
        if projectlife::util::now_ms() - started > timeout_ms {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// FR-WCH-4, the headline: a notification starts an out-of-band pass, and it is the notification and
// not the interval that explains the version — the interval is set to 60 s, so a version that
// appears within seconds can only have been triggered. The lower bound is the debounce: a version
// stored in less than the debounce would mean the trigger ignores FR-WCH-6.
#[test]
fn a_notification_starts_a_pass_long_before_the_interval() {
    let mut f = setup("trigfast", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "the daemon must report an installed watch set: {st}");
    assert_eq!(st.get("mode").and_then(|v| v.as_str()), Some("inotify"), "trigger mode: {st}");
    std::thread::sleep(std::time::Duration::from_millis(300));

    write(&f.project_root.join("src/a.txt"), b"two\n");
    let latency = wait_for_current_bytes(&f, "src/a.txt", 12_000);
    let st = wait_for_trigger_count(&f, d.id(), 1, 5_000);
    stop_daemon(&mut d);

    let latency = latency.unwrap_or_else(|| {
        panic!("with interval 60 s only a notification can explain a version this early; state after the wait: {st}")
    });
    assert!(
        latency < 12_000,
        "a notification must start a pass long before the 60 s interval, took {latency} ms"
    );
    assert!(
        latency >= 1000,
        "the debounce (1.5 s of quiet) must delay the pass; {latency} ms means it was ignored: {st}"
    );
    assert!(st_u64(&st, "triggerCycles") >= 1, "the pass must be recorded as trigger-driven: {st}");
    assert_eq!(st_u64(&st, "periodicCycles"), 1, "no periodic pass may have run inside that window: {st}");
}

// FR-WCH-4/FR-WCH-6 (requirement 4 and 6): notifications are lost, versions are not. The injector
// throws every notification away, so whatever is stored was found by the periodic pass alone.
// The assertions are about the injector being real (lost > 0, no trigger cycle claimed) and about
// the archive being complete.
#[test]
fn lost_notifications_do_not_lose_versions() {
    let mut f = setup("triglost", &[("src/a.txt", b"one\n"), ("src/b.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 1);
    let mut d = spawn_daemon(&f, &[("PROJECTLIFE_DROP_EVENTS", "1")], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "the watch set must still be installed: {st}");
    std::thread::sleep(std::time::Duration::from_millis(300));

    write(&f.project_root.join("src/a.txt"), b"two\n");
    write(&f.project_root.join("src/b.txt"), b"two\n");
    let la = wait_for_current_bytes(&f, "src/a.txt", 12_000);
    let lb = wait_for_current_bytes(&f, "src/b.txt", 12_000);
    // The counters are published at the end of a pass; wait for the injector's own number rather
    // than reading whatever the previous pass left in the file.
    let st = wait_for_state(&f, d.id(), 12_000, "lostEvents >= 1", |s| st_u64(s, "lostEvents") >= 1);
    stop_daemon(&mut d);

    assert!(la.is_some(), "a change made while notifications were lost must still be stored (a.txt)");
    assert!(lb.is_some(), "…and so must the second one (b.txt)");
    assert!(st_u64(&st, "lostEvents") > 0, "the injector must really have thrown notifications away: {st}");
    assert_eq!(
        st_u64(&st, "triggerCycles"),
        0,
        "with every notification dropped, no pass may claim to be trigger-driven: {st}"
    );
    assert!(st_u64(&st, "periodicCycles") >= 1, "the periodic pass is what stored them: {st}");
    archive_consistent(&f).expect("the archive must stay consistent without notifications");
}

// FR-WCH-4 + FR-WCH-3 (requirement 7): a trigger cycle and the periodic pass must never duplicate a
// version. Both kinds of pass really run here — that is asserted — and the journal must hold exactly
// one `put` per distinct content.
#[test]
fn a_notification_pass_and_the_periodic_pass_do_not_duplicate_a_version() {
    let mut f = setup("trigdup", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    // Interval 5 s, debounce 1.5 s: the trigger wins the race (it fires at ~1.6 s) and the periodic
    // pass still comes, so this test sees BOTH kinds of pass. With the default 5 s interval and the
    // 1.5 s debounce that is the normal configuration, not a special one.
    set_interval(&f, 5);
    let mut d = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    write(&f.project_root.join("src/a.txt"), b"two\n");
    assert!(
        wait_for_current_bytes(&f, "src/a.txt", 3_000).is_some(),
        "the first change must be stored by the trigger, before the 5 s periodic pass"
    );
    std::thread::sleep(std::time::Duration::from_millis(6000));
    write(&f.project_root.join("src/a.txt"), b"three\n");
    assert!(wait_for_current_bytes(&f, "src/a.txt", 3_000).is_some(), "the second change must be stored too");
    // More passes over an unchanged file: neither kind may write anything.
    wait_for_trigger_count(&f, d.id(), 2, 5_000);
    std::thread::sleep(std::time::Duration::from_millis(6000));
    let st = watch_state(&f);
    stop_daemon(&mut d);

    let puts: Vec<(String, String)> = journal_puts(&f).into_iter().filter(|(p, _)| p == "src/a.txt").collect();
    assert_eq!(
        puts.len(),
        3,
        "one version per distinct content (initial + two changes), no duplicate from the extra passes: {puts:?}"
    );
    let mut hashes: Vec<String> = puts.iter().map(|(_, h)| h.clone()).collect();
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), 3, "the three versions must be three different contents: {puts:?}");
    assert!(st_u64(&st, "triggerCycles") >= 2, "both changes must have been picked up by the trigger: {st}");
    assert!(st_u64(&st, "periodicCycles") >= 2, "…and the periodic passes must have run too: {st}");
    archive_consistent(&f).expect("the archive must stay consistent");
}

// FR-WCH-12 (requirement 5): two daemons may not write at the same time. The second one exits with a
// clear message; the first one keeps working (asserted by a version it stores afterwards).
#[test]
fn a_second_daemon_exits_with_a_clear_message_and_the_first_keeps_working() {
    let mut f = setup("trig2d", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d1 = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d1.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");
    let pid = d1.id();
    let lockfile = f.arch.root.join(".daemon");
    let mut held = false;
    for _ in 0..100 {
        let same = fs::read_to_string(&lockfile)
            .map(|t| projectlife::archive::parse_lock(&t).pid == Some(pid as i32))
            .unwrap_or(false);
        if same {
            held = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(held, "the running daemon must hold the daemon lock: {:?}", fs::read_to_string(&lockfile));

    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let out = run_bin(&home, &["--archive", &archive, "daemon", "run"], &[]);
    assert!(
        !out.status.success(),
        "a second daemon must refuse to start: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let msg = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(msg.contains("another daemon"), "the refusal must say what is wrong: {msg}");

    // The first daemon is untouched by the refused start.
    write(&f.project_root.join("src/a.txt"), b"two\n");
    let latency = wait_for_current_bytes(&f, "src/a.txt", 12_000);
    stop_daemon(&mut d1);
    assert!(latency.is_some(), "the first daemon must keep observing after the refused second start");
    assert!(!lockfile.exists(), "a clean stop must release the daemon lock");
}

// The daemon-lifetime file after `kill -9`: the next start takes it over instead of refusing for
// ever, and says so in the log.
#[test]
fn a_daemon_killed_with_sigkill_is_taken_over_by_the_next_start() {
    let mut f = setup("trigkill", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d1 = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d1.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");
    let lockfile = f.arch.root.join(".daemon");
    let mut held = false;
    for _ in 0..100 {
        if lockfile.is_file() {
            held = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(held, "the daemon lock must exist while the daemon runs");
    d1.kill().unwrap();
    d1.wait().unwrap();
    assert!(lockfile.is_file(), "kill -9 must leave the daemon lock behind (that is the case tested here)");

    let mut d2 = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d2.id(), 10_000);
    let log = fs::read_to_string(f.arch.root.join("logs/projectlife.log")).unwrap_or_default();
    write(&f.project_root.join("src/a.txt"), b"two\n");
    let latency = wait_for_current_bytes(&f, "src/a.txt", 12_000);
    stop_daemon(&mut d2);

    assert!(st_u64(&st, "watchedDirs") > 0, "the next start must work after a kill -9: {st}");
    assert!(log.contains("took over a stale daemon lock"), "the takeover must be logged: {log}");
    assert!(latency.is_some(), "the restarted daemon must observe");
}

// The cost rule behind FR-WCH-4: an excluded directory is named, not watched — and the negative half
// is controlled by a change in a tracked directory that IS seen.
#[test]
fn an_excluded_directory_is_not_watched_and_its_changes_trigger_nothing() {
    let mut files: Vec<(String, Vec<u8>)> = vec![("src/a.txt".into(), b"one\n".to_vec())];
    for i in 0..200 {
        files.push((format!("node_modules/dep/f{i}.js"), format!("dep {i}\n").into_bytes()));
    }
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let mut f = setup("trigexc", &borrowed);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");
    // Root + src. The 200 dependency files add no watch: that is the number this test is about.
    assert_eq!(
        st_u64(&st, "watchedDirs"),
        2,
        "only the project root and src may be watched, not node_modules: {st}"
    );
    assert_eq!(st_u64(&st, "events"), 0, "nothing has changed yet: {st}");

    // The positive control runs first: a change in a tracked directory must be seen. Then a change
    // inside the excluded one must add nothing at all.
    std::thread::sleep(std::time::Duration::from_millis(300));
    write(&f.project_root.join("src/a.txt"), b"two\n");
    let control = wait_for_current_bytes(&f, "src/a.txt", 12_000);
    assert!(control.is_some(), "positive control: a change in a tracked directory must be triggered");
    let st = wait_for_trigger_count(&f, d.id(), 1, 5_000);
    let events_after_control = st_u64(&st, "events");
    let triggers_after_control = st_u64(&st, "triggerCycles");
    assert!(events_after_control > 0, "positive control: the event must have been seen: {st}");
    assert!(triggers_after_control >= 1, "positive control: the pass must be trigger-driven: {st}");

    for i in 0..20 {
        write(&f.project_root.join(format!("node_modules/dep/f{i}.js")), format!("changed {i}\n").as_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(3000));
    let st = watch_state(&f);
    stop_daemon(&mut d);

    assert_eq!(
        st_u64(&st, "events"),
        events_after_control,
        "changes inside an excluded directory must produce no notification at all: {st}"
    );
    assert_eq!(
        st_u64(&st, "triggerCycles"),
        triggers_after_control,
        "…and therefore no trigger-driven pass: {st}"
    );
    assert_eq!(
        st_u64(&st, "periodicCycles"),
        1,
        "with interval 60 s and no tracked change, only the startup pass may have run: {st}"
    );
}

// FR-WCH-4: a directory created while the daemon runs is watched at once, so a file created inside
// it cannot be missed by the trigger.
#[test]
fn a_directory_created_while_the_daemon_runs_is_watched_at_once() {
    let mut f = setup("trignewdir", &[("src/a.txt", b"one\n")]);
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    let before = st_u64(&st, "watchedDirs");
    assert!(before > 0, "{st}");
    std::thread::sleep(std::time::Duration::from_millis(300));

    fs::create_dir_all(f.project_root.join("src/nd")).unwrap();
    write(&f.project_root.join("src/nd/inner.txt"), b"inner\n");
    let latency = wait_for_current_bytes(&f, "src/nd/inner.txt", 12_000);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let st = watch_state(&f);
    stop_daemon(&mut d);

    assert!(
        latency.is_some(),
        "a file created in a brand-new directory must be stored by the trigger, not only by the 60 s pass: {st}"
    );
    assert!(
        st_u64(&st, "watchedDirs") > before,
        "the new directory must join the watch set: before {before}, after {st}"
    );
}

// FR-WCH-1/FR-WCH-4 (requirement 1 and 2): notifications are a trigger, NOT a replacement — the
// periodic pass keeps its own rhythm with notifications on, and also with them off.
#[test]
fn the_periodic_pass_keeps_running_with_notifications_on_and_with_them_off() {
    for no_watch in [false, true] {
        let mut f = setup("trigperiodic", &[("src/a.txt", b"one\n")]);
        scan_initial(&mut f);
        set_interval(&f, 1);
        let mut d = spawn_daemon(&f, &[], no_watch);
        let st = if no_watch { wait_for_daemon_first_pass(&f, d.id(), 10_000) } else { wait_for_watches(&f, d.id(), 10_000) };
        if no_watch {
            assert_eq!(st.get("mode").and_then(|v| v.as_str()), Some("off"), "--no-watch: {st}");
        } else {
            assert_eq!(st.get("mode").and_then(|v| v.as_str()), Some("inotify"), "{st}");
        }
        // Nothing changes at all: only the periodic pass can be responsible for these cycles.
        std::thread::sleep(std::time::Duration::from_millis(4500));
        let st = watch_state(&f);
        stop_daemon(&mut d);
        assert!(
            st_u64(&st, "periodicCycles") >= 3,
            "the periodic pass must keep running (no_watch={no_watch}): {st}"
        );
        assert_eq!(st_u64(&st, "triggerCycles"), 0, "nothing changed, so no trigger may be claimed: {st}");
        let hb = Archive::open(&f.arch.root).unwrap().heartbeat_ms().unwrap_or(0);
        assert!(projectlife::util::now_ms() - hb < 60_000, "the heartbeat must be fresh: {hb}");
    }
}

// Requirement 4, second route: the kernel's own overflow signal. The test refuses to drain the
// inotify queue for four seconds while the same 500 files are rewritten many times, so the kernel
// queue really overflows (this is not a simulated overflow — the daemon's own overflow counter has to
// move). What must happen then: the loss is reported, and no file's final state is lost, because the
// pass the signal starts is a full pass.
#[test]
fn a_queue_overflow_is_reported_and_costs_no_version() {
    let max_queued = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // A first write to a file costs ~3 events, every rewrite ~2. Rewriting 500 files `writes` times
    // therefore pushes far more events than the kernel queue holds, while the archive only ever has
    // to store 500 files afterwards — the queue is what is being overflowed, not the disk.
    let files = 500usize;
    let writes = if max_queued > 0 { (max_queued / (files * 2)).max(8) + 4 } else { 24 };
    let mut f = setup("trigoverflow", &[("src/a.txt", b"one\n")]);
    fs::create_dir_all(f.project_root.join("src/bulk")).unwrap();
    write(&f.project_root.join("src/bulk/seed.txt"), b"seed\n");
    scan_initial(&mut f);
    set_interval(&f, 60);
    let mut d = spawn_daemon(&f, &[("PROJECTLIFE_DRAIN_DELAY_MS", "4000")], false);
    let st = wait_for_watches(&f, d.id(), 15_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    for w in 0..writes {
        for i in 0..files {
            write(&f.project_root.join(format!("src/bulk/f{i:04}.txt")), format!("bulk {i} write {w}\n").as_bytes());
        }
    }
    let st = wait_for_state(&f, d.id(), 40_000, "overflows >= 1", |s| st_u64(s, "overflows") >= 1);
    assert!(
        max_queued == 0 || st_u64(&st, "overflows") >= 1,
        "rewriting {files} files {writes} times without draining must overflow the kernel queue \
         (max_queued_events={max_queued}): {st}"
    );

    // Every file's FINAL content must be in the archive, found by the pass the overflow started.
    let started = projectlife::util::now_ms();
    let mut missing: Vec<String> = Vec::new();
    loop {
        let state = state_now(&f);
        missing = (0..files)
            .map(|i| format!("src/bulk/f{i:04}.txt"))
            .filter(|p| {
                let on_disk = hash_of(&f.project_root.join(p));
                state.get(p).map(|fst| fst.hash != on_disk).unwrap_or(true)
            })
            .collect();
        if missing.is_empty() || projectlife::util::now_ms() - started > 60_000 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let st_after = watch_state(&f);
    stop_daemon(&mut d);

    assert!(
        missing.is_empty(),
        "{} of {files} files were not stored with their final content after the overflow: {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );
    assert!(
        st_after.get("mode").and_then(|v| v.as_str()) == Some("inotify"),
        "the trigger must still be installed: {st_after}"
    );
    archive_consistent(&f).expect("the archive must stay consistent after an overflow");
}

// FR-WCH-6 (requirement 3): a process that writes continuously cannot postpone storage. The
// measured interval — the number `status` prints — must stay inside the promise of 2 × interval.
#[test]
fn a_continuously_written_file_is_stored_and_the_observed_interval_stays_within_twice_the_interval() {
    let mut f = setup("trigbusy", &[("src/a.txt", b"v0\n")]);
    scan_initial(&mut f);
    set_interval(&f, 2);
    let mut d = spawn_daemon(&f, &[], false);
    let st = wait_for_watches(&f, d.id(), 10_000);
    assert!(st_u64(&st, "watchedDirs") > 0, "{st}");

    let started = projectlife::util::now_ms();
    let mut contents: Vec<Vec<u8>> = Vec::new();
    for i in 1..=25 {
        let body = format!("v{i}\n").into_bytes();
        contents.push(body.clone());
        write(&f.project_root.join("src/a.txt"), &body);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let mid = journal_puts(&f).into_iter().filter(|(p, _)| p == "src/a.txt").count();
    let elapsed = projectlife::util::now_ms() - started;
    write(&f.project_root.join("src/a.txt"), b"final\n");
    assert!(wait_for_current_bytes(&f, "src/a.txt", 12_000).is_some(), "the final content must be stored");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    stop_daemon(&mut d);

    assert!(
        mid >= 1,
        "a continuously written file must still be stored while it is being written, after {elapsed} ms \
         there was no version at all — the debounce postponed storage"
    );
    let puts: Vec<(String, String)> = journal_puts(&f).into_iter().filter(|(p, _)| p == "src/a.txt").collect();
    let mut hashes: Vec<String> = puts.iter().map(|(_, h)| h.clone()).collect();
    let total = hashes.len();
    hashes.sort();
    hashes.dedup();
    assert_eq!(total, hashes.len(), "no content may be stored twice: {puts:?}");
    let p = Project::from_dir(&f.project.dir).unwrap();
    let med = p.meta.get("observedIntervalMs").and_then(|v| v.get("median")).and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(med > 0, "the observed interval must have been measured");
    assert!(
        med <= 2 * 2000,
        "FR-WCH-6: the measured observation interval {med} ms must stay within 2 x interval (4000 ms)"
    );
}

// ============================================================================================
// Round 291 — universal mode: presets and auto-detection (SPEC §6.17, AT-39 … AT-48)
// ============================================================================================

fn cands(d: &projectlife::detect::Detection) -> Vec<(String, u32)> {
    d.candidates.iter().map(|c| (c.id.clone(), c.confidence)).collect()
}

fn preset_filter(d: &projectlife::detect::Detection) -> FilterConfig {
    let mut settings = serde_json::Map::new();
    settings.insert("filterMode".into(), serde_json::json!("allow"));
    settings.insert("include".into(), serde_json::json!(d.include_globs));
    settings.insert("exclude".into(), serde_json::json!(d.exclude_globs));
    settings.insert("maxFileSizeKb".into(), serde_json::json!(d.max_file_size_kb));
    FilterConfig::for_profile("preset", &settings)
}

fn project_by_name(arch: &Archive, name: &str) -> Project {
    arch.load_projects()
        .unwrap()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no project named {name}"))
}

fn tracked_paths(p: &Project) -> std::collections::BTreeSet<String> {
    let j = events::load_journal(&p.dir).unwrap();
    j.events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .filter_map(|e| events::get_str(e, "path"))
        .collect()
}

fn write_sparse(path: &Path, len: u64) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    let f = fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
}

// AT-39: an office folder is recognised, and only the documents are protected.
#[test]
fn at39_office_folder_detection() {
    let f = setup(
        "at39",
        &[
            ("Documents/report.docx", b"doc"),
            ("Documents/table.xlsx", b"xls"),
            ("Documents/slides.pptx", b"ppt"),
            ("Documents/notes.txt", b"txt"),
            ("Documents/archive.zip", b"zip"),
            ("Documents/video.mp4", b"mp4"),
            ("Documents/program.exe", b"exe"),
        ],
    );
    let d = projectlife::detect::detect_profile(&f.project_root.join("Documents"));
    assert_eq!(d.best_id().as_deref(), Some("office"), "candidates: {:?}", cands(&d));
    assert!(d.confidence() > 80, "office confidence was {} (candidates {:?})", d.confidence(), cands(&d));
    for g in ["*.docx", "*.xlsx", "*.pptx", "*.txt"] {
        assert!(d.include_globs.iter().any(|x| x == g), "{g} must be suggested: {:?}", d.include_globs);
    }
    for g in ["*.zip", "*.mp4", "*.exe"] {
        assert!(!d.include_globs.iter().any(|x| x == g), "{g} must not be suggested");
    }
    // the filter the preset writes agrees with the suggestion
    let filter = preset_filter(&d);
    let own = projectlife::glob::IgnoreRules::empty();
    for p in ["report.docx", "table.xlsx", "slides.pptx", "notes.txt"] {
        assert!(filter.decide_file(p, 4, &own, &own).track, "{p} must be tracked");
    }
    for p in ["archive.zip", "video.mp4", "program.exe"] {
        let dec = filter.decide_file(p, 4, &own, &own);
        assert!(!dec.track, "{p} must not be tracked");
        assert_eq!(dec.reason.as_deref(), Some("not_in_preset"), "{p} reason");
    }
}

// AT-40: a developer folder is recognised; dependencies and build output are never counted.
#[test]
fn at40_developer_folder_detection() {
    let f = setup(
        "at40",
        &[
            ("app/package.json", b"{}"),
            ("app/src/app.ts", b"x"),
            ("app/src/util.ts", b"x"),
            ("app/README.md", b"x"),
            ("app/node_modules/dep/index.js", b"x"),
            ("app/dist/bundle.js", b"x"),
            ("app/.git/config", b"x"),
        ],
    );
    let d = projectlife::detect::detect_profile(&f.project_root.join("app"));
    assert_eq!(d.best_id().as_deref(), Some("developer"), "candidates: {:?}", cands(&d));
    assert!(d.confidence() > 90, "developer confidence was {}", d.confidence());
    assert!(!d.extension_counts.contains_key(".js"), "files inside node_modules/dist are not counted: {:?}", d.extension_counts);
    assert!(d.skipped_dirs.iter().any(|(p, r)| p == "node_modules" && r == "dependency"));
    assert!(d.skipped_dirs.iter().any(|(p, r)| p == "dist" && r == "dependency"));
    assert!(d.skipped_dirs.iter().any(|(p, _)| p == ".git"));
    assert!(d.include_globs.iter().any(|x| x == "*.ts"));
    let filter = preset_filter(&d);
    let own = projectlife::glob::IgnoreRules::empty();
    assert!(filter.decide_file("src/app.ts", 4, &own, &own).track);
}

// AT-41: a designer folder is recognised, and a temporary file is excluded by the hard rule.
#[test]
fn at41_designer_folder_detection() {
    let f = setup(
        "at41",
        &[
            ("Design/layout.psd", b"x"),
            ("Design/icon.svg", b"x"),
            ("Design/photo.jpg", b"x"),
            ("Design/cache.tmp", b"x"),
        ],
    );
    let d = projectlife::detect::detect_profile(&f.project_root.join("Design"));
    assert_eq!(d.best_id().as_deref(), Some("designer"), "candidates: {:?}", cands(&d));
    for g in ["*.psd", "*.svg", "*.jpg"] {
        assert!(d.include_globs.iter().any(|x| x == g), "{g} must be suggested");
    }
    assert!(
        d.safety_excluded.iter().any(|(p, r, _)| p == "cache.tmp" && r == "temporary"),
        "a .tmp file is excluded by the hard rule: {:?}",
        d.safety_excluded
    );
    assert!(!d.extension_counts.contains_key(".tmp"), "temporary files are not part of the profile evidence");
    let filter = preset_filter(&d);
    let own = projectlife::glob::IgnoreRules::empty();
    let dec = filter.decide_file("cache.tmp", 4, &own, &own);
    assert!(!dec.track);
    assert_eq!(dec.reason.as_deref(), Some("temporary"));
}

// AT-42: a genuinely mixed folder is not decided for the user.
#[test]
fn at42_mixed_folder_low_confidence() {
    let f = setup(
        "at42",
        &[
            ("mixed/report.docx", b"x"),
            ("mixed/app.ts", b"x"),
            ("mixed/photo.jpg", b"x"),
            ("mixed/song.mp3", b"x"),
        ],
    );
    let root = f.project_root.join("mixed");
    let d = projectlife::detect::detect_profile(&root);
    assert!(d.candidates.iter().all(|c| c.confidence <= 40), "no profile may claim this folder: {:?}", cands(&d));
    assert!(d.suggest_custom, "custom must be suggested: {:?}", cands(&d));
    assert_eq!(d.suggested(), "custom");
    assert!(d.include_globs.is_empty(), "nothing is auto-selected for an undecided folder");
    assert!(d.warnings.iter().any(|w| w.contains("no profile reached a convincing confidence")));

    // and the CLI says the same thing in its JSON
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let out = run_bin(&home, &["detect", root.to_str().unwrap(), "--json"], &[]);
    assert!(out.status.success(), "detect must succeed: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["suggestCustom"], serde_json::json!(true));
    assert!(v["confidence"].as_u64().unwrap() <= 40, "cli confidence: {}", v["confidence"]);
}

// AT-43: secrets are never included automatically — not even by a forced include list.
#[test]
fn at43_secrets_never_auto_included() {
    let f = setup(
        "at43",
        &[
            ("secrets/.env", b"SECRET=1\n"),
            ("secrets/id_rsa", b"PRIVATE\n"),
            ("secrets/credentials.json", b"{\"k\":1}\n"),
            ("secrets/config.json", b"{}\n"),
        ],
    );
    let d = projectlife::detect::detect_profile(&f.project_root.join("secrets"));
    let excluded: Vec<String> = d.safety_excluded.iter().map(|(p, _, _)| p.clone()).collect();
    for p in [".env", "id_rsa", "credentials.json"] {
        assert!(excluded.contains(&p.to_string()), "{p} must be reported as excluded: {excluded:?}");
    }
    assert!(!excluded.contains(&"config.json".to_string()), "an ordinary .json is not a secret");
    assert!(d.warnings.iter().any(|w| w.contains("safety rules")), "a warning is shown: {:?}", d.warnings);

    // A profile that includes *.json must still not touch the secrets.
    let mut settings = serde_json::Map::new();
    settings.insert("filterMode".into(), serde_json::json!("allow"));
    settings.insert("include".into(), serde_json::json!(["*.json", ".env", "id_rsa", "credentials.json"]));
    let filter = FilterConfig::for_profile("preset", &settings);
    let own = projectlife::glob::IgnoreRules::empty();
    assert!(filter.decide_file("config.json", 4, &own, &own).track);
    for p in [".env", "id_rsa", "credentials.json"] {
        let dec = filter.decide_file(p, 4, &own, &own);
        assert!(!dec.track, "{p} must stay out even when the include list names it");
        assert_eq!(dec.reason.as_deref(), Some("secret"), "{p} reason");
    }
    // control: the one flag that is allowed to change this does change it
    settings.insert("includeSecrets".into(), serde_json::json!(true));
    let open = FilterConfig::for_profile("preset", &settings);
    assert!(open.decide_file(".env", 4, &own, &own).track, "the control must bite");
}

// AT-44: detection reads metadata only — measured three ways, one of them impossible to fake.
#[test]
fn at44_detection_does_not_read_content() {
    let f = setup("at44", &[]);
    let root = f.project_root.join("many");
    fs::create_dir_all(&root).unwrap();
    for i in 0..1000 {
        write(&root.join(format!("file{i}.txt")), &vec![b'x'; 10 * 1024]);
    }
    // an old atime on every file: any read updates it (relatime), and nothing else does
    for i in 0..1000 {
        let p = root.join(format!("file{i}.txt"));
        let st = std::process::Command::new("touch").args(["-a", "-t", "200101010000"]).arg(&p).status().unwrap();
        assert!(st.success());
    }
    let fifo = root.join("pipe.psd");
    let st = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(st.success(), "the test needs mkfifo");

    let before: Vec<std::time::SystemTime> = (0..20)
        .map(|i| fs::metadata(root.join(format!("file{i}.txt"))).unwrap().accessed().unwrap())
        .collect();

    let (tx, rx) = std::sync::mpsc::channel();
    let probe = root.clone();
    std::thread::spawn(move || {
        let d = projectlife::detect::detect_profile(&probe);
        let _ = tx.send(d);
    });
    let d = match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(d) => d,
        Err(_) => panic!("detection did not return: it opened a FIFO for reading"),
    };
    assert!(d.elapsed_ms < 2000, "1000 files took {} ms — the limit is 2 s", d.elapsed_ms);
    assert_eq!(d.files, 1000, "the ordinary files are counted");
    assert_eq!(d.special, 1, "the FIFO is counted as a special file, never opened: {:?}", d);

    for (i, was) in before.iter().enumerate() {
        let now = fs::metadata(root.join(format!("file{i}.txt"))).unwrap().accessed().unwrap();
        assert_eq!(&now, was, "file{i}.txt was read: its atime moved from {was:?} to {now:?}");
    }
    // positive control: the signal does move when a file really is read on this filesystem
    let probe_file = root.join("file0.txt");
    let was = fs::metadata(&probe_file).unwrap().accessed().unwrap();
    let _ = fs::read(&probe_file).unwrap();
    let now = fs::metadata(&probe_file).unwrap().accessed().unwrap();
    assert_ne!(now, was, "atime does not move on read here, so the check above proves nothing");
}

// AT-45: a named preset wins over detection.
#[test]
fn at45_manual_preset_override() {
    let f = setup("at45", &[]);
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let root = f.base.join("mixed");
    write(&root.join("report.docx"), b"x");
    write(&root.join("app.ts"), b"x");
    write(&root.join("photo.jpg"), b"x");
    write(&root.join("song.mp3"), b"x");
    let archive = f.arch.root.to_str().unwrap().to_string();

    // auto-detection would refuse to decide here; the user decides instead
    let auto = projectlife::detect::detect_profile(&root);
    assert!(auto.suggest_custom, "the fixture must be undecidable by detection: {:?}", cands(&auto));

    let out = run_bin(&home, &["--archive", &archive, "add", root.to_str().unwrap(), "--name", "forced", "--preset", "developer", "--yes"], &[]);
    assert!(out.status.success(), "add --preset developer: {}", String::from_utf8_lossy(&out.stderr));
    let p = project_by_name(&f.arch, "forced");
    assert_eq!(p.meta["preset"]["source"], serde_json::json!("manual"));
    assert_eq!(p.meta["presetSource"], serde_json::json!("manual"));
    assert_eq!(p.meta["preset"]["id"], serde_json::json!("developer"));
    assert_eq!(p.meta["profile"], serde_json::json!("preset"));
    let include: Vec<String> = p.meta["settings"]["include"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert!(include.iter().any(|g| g == "*.ts"), "{include:?}");
    assert!(!include.iter().any(|g| g == "*.docx"), "the office profile must not be in force");
    assert_eq!(p.meta["settings"]["filterMode"], serde_json::json!("allow"));
    // the journal says why, in the profile's own words
    let j = events::load_journal(&p.dir).unwrap();
    let ev = j.events.iter().find(|e| events::event_type(e) == "filters").expect("a filters event");
    assert_eq!(events::get_str(ev, "reason").as_deref(), Some("preset_applied"));
    assert_eq!(events::get_str(ev, "preset").as_deref(), Some("developer"));
}

// AT-46: the user edits the suggestion, and the edit really changes what is stored.
#[test]
fn at46_user_edits_preset() {
    let f = setup("at46", &[]);
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let root = f.base.join("Documents");
    write(&root.join("report.docx"), b"doc");
    write(&root.join("slides.pptx"), b"ppt");
    write(&root.join("notes.txt"), b"txt");
    write(&root.join("README.md"), b"readme");
    let archive = f.arch.root.to_str().unwrap().to_string();

    let out = run_bin(
        &home,
        &[
            "--archive", &archive, "add", root.to_str().unwrap(), "--name", "edited", "--preset", "auto", "--yes",
            "--edit-add", ".md", "--edit-remove", ".pptx",
        ],
        &[],
    );
    assert!(out.status.success(), "add --preset auto --edit-*: {}", String::from_utf8_lossy(&out.stderr));
    let p = project_by_name(&f.arch, "edited");
    assert_eq!(p.meta["presetModifiedByUser"], serde_json::json!(true));
    assert_eq!(p.meta["preset"]["modifiedByUser"], serde_json::json!(true));
    let include: Vec<String> = p.meta["settings"]["include"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert!(include.iter().any(|g| g == "*.md"), "the added extension is in force: {include:?}");
    assert!(!include.iter().any(|g| g == "*.pptx"), "the removed extension is gone: {include:?}");
    let tracked = tracked_paths(&p);
    assert!(tracked.contains("README.md"), "the added extension is stored: {tracked:?}");
    assert!(!tracked.contains("slides.pptx"), "the removed extension is not stored: {tracked:?}");
}

// AT-47: large files are named, not included.
#[test]
fn at47_large_files_warning() {
    let f = setup("at47", &[("Large/small.txt", b"x")]);
    write_sparse(&f.project_root.join("Large/video.mp4"), 2_000_000_000);
    write_sparse(&f.project_root.join("Large/backup.zip"), 5_000_000_000);
    let d = projectlife::detect::detect_profile(&f.project_root.join("Large"));
    assert_eq!(d.large_files.len(), 2, "both large files are reported: {:?}", d.large_files);
    let w = "Found 2 files larger than 1 GB. They will be skipped unless you explicitly include large files.";
    assert!(d.warnings.iter().any(|x| x == w), "warnings were: {:?}", d.warnings);
    // and the size limit is a real limit: a forced include alone does not lift it
    let mut settings = serde_json::Map::new();
    settings.insert("filterMode".into(), serde_json::json!("allow"));
    settings.insert("include".into(), serde_json::json!(["*.mp4"]));
    settings.insert("maxFileSizeKb".into(), serde_json::json!(100u64));
    let own = projectlife::glob::IgnoreRules::empty();
    let filter = FilterConfig::for_profile("preset", &settings);
    let dec = filter.decide_file("video.mp4", 2_000_000_000, &own, &own);
    assert!(!dec.track);
    assert_eq!(dec.reason.as_deref(), Some("too_large"));
    settings.insert("includeLargeFiles".into(), serde_json::json!(true));
    let lifted = FilterConfig::for_profile("preset", &settings);
    assert!(lifted.decide_file("video.mp4", 2_000_000_000, &own, &own).track, "the control must bite");
}

// AT-48: extensions nobody knows are shown, counted, and never auto-included.
#[test]
fn at48_unknown_extensions() {
    let f = setup(
        "at48",
        &[("unknown/file.abc", b"x"), ("unknown/file.xyz", b"x"), ("unknown/file.unknown", b"x")],
    );
    let root = f.project_root.join("unknown");
    let d = projectlife::detect::detect_profile(&root);
    assert!(d.suggest_custom, "candidates: {:?}", cands(&d));
    for e in [".abc", ".xyz", ".unknown"] {
        assert!(d.unknown_extensions.iter().any(|(x, n)| x == e && *n == 1), "{e} missing from {:?}", d.unknown_extensions);
    }
    assert!(d.include_globs.is_empty(), "nothing is auto-included: {:?}", d.include_globs);

    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let out = run_bin(&home, &["detect", root.to_str().unwrap(), "--json"], &[]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<String> = v["unknownExtensions"].as_array().unwrap().iter().map(|u| u["extension"].as_str().unwrap().to_string()).collect();
    for e in [".abc", ".xyz", ".unknown"] {
        assert!(names.contains(&e.to_string()), "{e} missing from the CLI report: {names:?}");
    }
    assert!(v["suggestedInclude"].as_array().unwrap().is_empty());
}
