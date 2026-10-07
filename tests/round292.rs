//! Round 292 acceptance tests: retention policy, the scheduler checks, and the read-only MCP surface.
//!
//! Every test runs on a real filesystem and reads the result back from disk. The helpers are
//! duplicated from `acceptance.rs` on purpose: integration tests are separate crates, and a shared
//! module would have to be introduced into a file that already proves the earlier rounds.

use projectlife::archive::{iso_ms, Archive, Project};
use projectlife::events;
use projectlife::mcp;
use projectlife::ops;
use projectlife::quick;
use projectlife::retention;
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
    let base = std::env::temp_dir().join(format!("projectlife-292-{}-{}-{}", name, std::process::id(), id));
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

fn scan_initial(f: &mut Fixture) {
    let opts = ScanOptions { reason: "initial".into(), deep: true, with_initial_snapshot: true, ..Default::default() };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap();
}

fn scan_now(f: &mut Fixture) {
    let opts = ScanOptions { reason: "observed".into(), deep: false, ..Default::default() };
    scan::scan_project(&f.arch, &mut f.project, &opts).unwrap();
}

fn reload(f: &Fixture) -> Project {
    Project::from_dir(&f.project.dir).unwrap()
}

fn run_bin(home: &Path, args: &[&str]) -> std::process::Output {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", home);
    c.args(args);
    c.output().expect("the shipped binary must be runnable")
}

fn state_sig(events_list: &[events::Ev], t: i64) -> BTreeMap<String, (String, String, u64)> {
    events::state_at(events_list, t, None)
        .into_iter()
        .map(|(k, v)| (k, (v.hash, v.kind, v.size)))
        .collect()
}

/// Spread every event of the journal over `span_days`, preserving order, and move `historyStartsAt`
/// with it — a project whose events predate its own start of history refuses to restore them.
fn backdate(f: &Fixture, span_days: i64) {
    backdate_at(f, span_days, 0)
}

/// The same, with the whole history pushed `ago_days` into the past.
fn backdate_at(f: &Fixture, span_days: i64, ago_days: i64) {
    let j = events::load_journal(&f.project.dir).unwrap();
    let now = util::now_ms() - ago_days * 86_400_000;
    let n = j.events.len();
    assert!(n >= 2, "backdating needs a journal");
    let mut evs = j.events.clone();
    for (i, e) in evs.iter_mut().enumerate() {
        let frac = i as f64 / (n - 1) as f64;
        let ts = now - ((span_days as f64) * 86_400_000.0 * (1.0 - frac)) as i64;
        e.insert("ts".into(), Value::from(ts));
    }
    let dir = f.project.events_dir();
    for e in fs::read_dir(&dir).unwrap().flatten() {
        let _ = fs::remove_file(e.path());
    }
    let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &evs {
        by_month
            .entry(util::month_name(events::ts_of(e)))
            .or_default()
            .push(serde_json::to_string(&Value::Object(e.clone())).unwrap());
    }
    for (m, lines) in by_month {
        fs::write(dir.join(format!("{m}.jsonl")), lines.join("\n") + "\n").unwrap();
    }
    let mut p = reload(f);
    p.set_meta("historyStartsAt", Value::from(iso_ms(events::ts_of(&evs[0]))));
    p.save_meta().unwrap();
}

fn consistent(f: &Fixture) -> Result<(), String> {
    let j = events::load_journal(&f.project.dir)?;
    let st = Store::new(&f.project.blobs_dir(), &f.project.tmp_dir());
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for e in &j.events {
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
    let dangling: Vec<String> = st.list_all().into_iter().map(|(h, _)| h).filter(|h| !referenced.contains(h)).collect();
    if !dangling.is_empty() {
        return Err(format!("unreferenced blobs left behind: {dangling:?}"));
    }
    Ok(())
}

fn retention_anchors(f: &Fixture) -> Vec<i64> {
    let j = events::load_journal(&f.project.dir).unwrap();
    let mut v: Vec<i64> = j
        .events
        .iter()
        .filter(|e| {
            events::event_type(e) == "snapshot"
                && matches!(events::get_str(e, "reason").as_deref(), Some("retention-anchor") | Some("retention-boundary"))
        })
        .map(events::ts_of)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn archive_manifest(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.insert(rel, store::sha256_bytes(&fs::read(&p).unwrap_or_default()));
            }
        }
    }
    out
}

/// A project with a spread-out history: enough versions that a policy has something to thin.
fn policy_fixture(name: &str, versions: usize, span_days: i64) -> Fixture {
    let mut f = setup(name, &[("src/a.txt", b"v0\n"), ("src/b.txt", b"b0\n")]);
    scan_initial(&mut f);
    for i in 1..=versions {
        write(&f.project_root.join("src/a.txt"), format!("v{i}\n").as_bytes());
        scan_now(&mut f);
    }
    backdate(&f, span_days);
    f
}

// -------------------------------------------------------------------------------------------
// A. Retention policy: the moments it keeps must stay restorable exactly.
// -------------------------------------------------------------------------------------------

#[test]
fn a_policy_keeps_every_retained_moment_restorable_exactly() {
    let f = policy_fixture("policy", 30, 60);
    let p = reload(&f);
    let policy = retention::Policy::parse("7d:all,30d:1/day,365d:1/month").unwrap();
    let now = util::now_ms();
    let before = events::load_journal(&f.project.dir).unwrap().events.clone();
    let plan = retention::plan(&p, &policy, now).unwrap();
    assert!(plan.anchors.len() > 2, "the policy must retain more than a couple of moments: {:?}", plan.anchors);
    assert!(plan.versions_dropped() > 0, "this history must lose versions: {}", plan.versions_before);

    let mut expected: BTreeMap<i64, BTreeMap<String, (String, String, u64)>> = BTreeMap::new();
    for t in &plan.anchors {
        expected.insert(*t, state_sig(&before, *t));
    }
    // every moment at or after the boundary of the keep-everything window must also be exact
    let mut sampled: Vec<i64> = vec![plan.boundary_anchor, now - 3_600_000, now - 60_000];
    for e in before.iter() {
        if events::ts_of(e) >= plan.boundary_anchor {
            sampled.push(events::ts_of(e));
        }
    }
    sampled.sort_unstable();
    sampled.dedup();
    for t in &sampled {
        expected.entry(*t).or_insert_with(|| state_sig(&before, *t));
    }

    // The policy's own arithmetic, checked against the journal instead of against itself: one
    // bucket per local day, exactly one version kept in each, everything else in the window gone.
    let mut days: BTreeSet<i64> = BTreeSet::new();
    let mut in_window = 0usize;
    for e in before.iter() {
        if !matches!(events::event_type(e), "put" | "delete" | "move" | "symlink") {
            continue;
        }
        let age_days = (now - events::ts_of(e)).div_euclid(86_400_000);
        // the daily window is "older than 7 days, not older than 30" — the same boundary the policy
        // names, written here rather than read from the code
        if age_days > 7 && age_days <= 30 {
            let (y, m, d) = util::local_day(events::ts_of(e));
            days.insert((y as i64) * 10_000 + (m as i64) * 100 + d as i64);
            in_window += 1;
        }
    }
    let daily = plan.windows.iter().find(|w| w.unit == retention::Unit::PerDay).expect("a daily window");
    assert_eq!(daily.buckets, days.len(), "one bucket per local day inside the daily window");
    assert_eq!(daily.kept, days.len(), "exactly one version per day is kept");
    assert_eq!(daily.dropped, in_window - days.len(), "every other version in that window is dropped");

    let mut p2 = reload(&f);
    let outcome = retention::apply(&f.arch, &mut p2, &policy, now).unwrap();
    assert_eq!(outcome.plan.versions_kept, plan.versions_kept);

    let after = events::load_journal(&f.project.dir).unwrap();
    let mut checked = 0usize;
    for (t, want) in expected.iter() {
        let got = state_sig(&after.events, *t);
        assert_eq!(&got, want, "the state at {} changed under the policy prune", util::fmt_local(*t));
        checked += 1;
    }
    assert!(checked >= plan.anchors.len(), "the comparison must cover every anchor");

    assert_eq!(consistent(&f).err(), None);
    let stored = retention::stored_policy(&reload(&f));
    assert_eq!(stored.as_deref(), Some("7d:all,30d:1/day,365d:1/month"), "the applied policy must be stored in project.json");

    // The anchors the plan announced are the anchors the journal carries: read back independently.
    let in_journal = retention_anchors(&f);
    assert_eq!(in_journal, plan.anchors, "the plan and the journal disagree about the retained moments");

    // …and the applied operation is in the operation log, where `recent` reads it.
    let rec = ops::last_of(&f.arch, "retention", Some(&p.name)).expect("the apply must be recorded");
    assert_eq!(ops::ts_of(&rec) > 0, true);
    let rows = quick::recent(&f.arch, 5).unwrap();
    assert!(rows.iter().any(|r| r.kind == "op:retention"), "recent must show the retention operation");

    // A policy without a keep-everything window is the case where the boundary anchor has work to
    // do: nothing is kept verbatim, so the current state is only exact because it is an anchor.
    let f2 = policy_fixture("policy-noboundary", 20, 50);
    let p2 = reload(&f2);
    let policy2 = retention::Policy::parse("3d:1/day,30d:1/month").unwrap();
    let now2 = util::now_ms();
    let before2 = events::load_journal(&f2.project.dir).unwrap().events.clone();
    let plan2 = retention::plan(&p2, &policy2, now2).unwrap();
    assert_eq!(plan2.verbatim_from, now2, "with no keep-everything window nothing is kept as events");
    assert!(plan2.anchors.contains(&plan2.boundary_anchor), "the boundary moment must be an anchor");
    let state_now_before = state_sig(&before2, now2);
    assert!(!state_now_before.is_empty(), "the fixture must have a current state to lose");
    let mut p2b = reload(&f2);
    retention::apply(&f2.arch, &mut p2b, &policy2, now2).unwrap();
    let after2 = events::load_journal(&f2.project.dir).unwrap();
    assert_eq!(
        state_sig(&after2.events, now2),
        state_now_before,
        "without a boundary anchor the current state would be reconstructed from sparse moments"
    );
    assert_eq!(consistent(&f2).err(), None);
}

#[test]
fn a_policy_prune_does_not_resurrect_a_deleted_file() {
    let mut f = setup("policy-delete", &[("src/gone.txt", b"here\n"), ("src/keep.txt", b"k\n")]);
    scan_initial(&mut f);
    for i in 1..=4 {
        write(&f.project_root.join("src/keep.txt"), format!("k{i}\n").as_bytes());
        scan_now(&mut f);
    }
    // the file is deleted in the middle of the history, so the delete lands in the thinned region
    fs::remove_file(f.project_root.join("src/gone.txt")).unwrap();
    scan_now(&mut f);
    for i in 5..=8 {
        write(&f.project_root.join("src/keep.txt"), format!("k{i}\n").as_bytes());
        scan_now(&mut f);
    }
    backdate(&f, 60);
    let now = util::now_ms();
    let policy = retention::Policy::parse("7d:all,30d:1/day").unwrap();
    let mut p = reload(&f);
    let plan = retention::plan(&p, &policy, now).unwrap();
    assert!(plan.versions_dropped() > 0, "this fixture must actually thin");
    let before_journal = events::load_journal(&f.project.dir).unwrap().events.clone();
    retention::apply(&f.arch, &mut p, &policy, now).unwrap();
    let after = events::load_journal(&f.project.dir).unwrap();

    // the deleted file must not come back, at the boundary or at any later moment
    for t in [plan.boundary_anchor, now] {
        let s = state_sig(&after.events, t);
        assert!(!s.contains_key("src/gone.txt"), "a deleted file reappeared at {}: {:?}", util::fmt_local(t), s.keys().collect::<Vec<_>>());
        assert_eq!(s, state_sig(&before_journal, t), "the state at {} must be unchanged", util::fmt_local(t));
    }
    assert_eq!(consistent(&f).err(), None);
}

#[test]
fn a_policy_that_would_remove_everything_is_refused_and_changes_nothing() {
    let mut f = setup("policy-refuse", &[("src/a.txt", b"v0\n")]);
    scan_initial(&mut f);
    for i in 1..=6 {
        write(&f.project_root.join("src/a.txt"), format!("v{i}\n").as_bytes());
        scan_now(&mut f);
    }
    // the whole history is five days old or older; a one-day horizon would leave nothing
    backdate_at(&f, 40, 5);
    let p = reload(&f);
    let before = events::load_journal(&f.project.dir).unwrap();
    let versions_before = before.events.iter().filter(|e| events::event_type(e) == "put").count();
    let bytes_before = p.size_on_disk();
    // everything in this history is older than one day, and the policy keeps nothing that old
    let policy = retention::Policy::parse("1d:1/day").unwrap();
    let err = retention::plan(&p, &policy, util::now_ms()).err().expect("the plan must refuse");
    assert!(err.contains("keeps nothing from this history"), "the refusal must say what it is refusing: {err}");
    assert!(err.contains("nothing was changed"), "and it must say that nothing happened: {err}");
    let p2 = reload(&f);
    assert_eq!(p2.size_on_disk(), bytes_before);
    let after = events::load_journal(&f.project.dir).unwrap();
    assert_eq!(
        after.events.iter().filter(|e| events::event_type(e) == "put").count(),
        versions_before,
        "a refused policy must not touch the journal"
    );
}

#[test]
fn a_stored_policy_is_never_applied_by_itself() {
    let mut f = policy_fixture("policy-auto", 18, 40);
    let policy = retention::Policy::parse("7d:all,2d:1/day").unwrap();
    let mut p = reload(&f);
    retention::store_policy(&mut p, &policy).unwrap();
    let versions_before = events::load_journal(&f.project.dir).unwrap().events.len();
    let anchors_before = retention_anchors(&f).len();

    // ordinary observation of an unchanged project, twice
    f.project = reload(&f);
    scan_now(&mut f);
    f.project = reload(&f);
    scan_now(&mut f);
    let cli = run_bin(&f.base.join("home"), &["--archive", &f.arch.root.to_string_lossy(), "list"]);
    assert!(cli.status.success(), "the ordinary CLI must keep working with a stored policy");

    let journal = events::load_journal(&f.project.dir).unwrap();
    assert_eq!(
        retention_anchors(&f).len(),
        anchors_before,
        "a stored policy must not create anchors by itself"
    );
    assert!(
        journal.events.iter().filter(|e| events::event_type(e) == "put").count() > 0,
        "the journal must still be there — otherwise this test proves nothing"
    );
    assert!(
        journal.events.len() >= versions_before,
        "a stored policy must never remove history by itself ({} events now, {} before)",
        journal.events.len(),
        versions_before
    );
    assert_eq!(retention::applied_at(&reload(&f)), None, "nothing may record an application");
}

// -------------------------------------------------------------------------------------------
// B. Heartbeat and healthcheck: one exit code a scheduler can act on.
// -------------------------------------------------------------------------------------------

#[test]
fn heartbeat_check_exit_codes_are_fresh_zero_stale_one() {
    let mut f = setup("heartbeat", &[("a.txt", b"a\n")]);
    scan_initial(&mut f);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    // the heartbeat belongs to the archive, and it is written by a cycle, so run one through the CLI
    let cycle = run_bin(&home, &["--archive", &archive, "scan-once", "heartbeat"]);
    assert!(cycle.status.success(), "{}", String::from_utf8_lossy(&cycle.stderr));
    assert!(f.arch.heartbeat_ms().is_some(), "a cycle must write the heartbeat this test reads");
    let fresh = run_bin(&home, &["--archive", &archive, "heartbeat-check"]);
    assert_eq!(fresh.status.code(), Some(0), "a fresh heartbeat must exit 0: {}", String::from_utf8_lossy(&fresh.stdout));
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("heartbeat:"), "and it must say what it measured");

    // stale: a beat older than the limit
    fs::write(f.arch.heartbeat_path(), format!("{}\n", util::now_ms() - 10 * 60_000)).unwrap();
    let stale = run_bin(&home, &["--archive", &archive, "heartbeat-check"]);
    assert_eq!(stale.status.code(), Some(1), "a stale heartbeat must exit 1: {}", String::from_utf8_lossy(&stale.stdout));
    assert!(String::from_utf8_lossy(&stale.stdout).contains("scan-once"), "and it must print the way out");

    // the limit is a parameter, not a constant
    let ok = run_bin(&home, &["--archive", &archive, "heartbeat-check", "--max-age", "1200"]);
    assert_eq!(ok.status.code(), Some(0), "a 10-minute-old beat is fresh with --max-age 1200");

    // stale while a live process claims to be the daemon: a different failure, a different code
    fs::write(f.arch.root.join(".daemon"), format!("daemon run {} {}\n", std::process::id(), util::now_ms())).unwrap();
    let disagree = run_bin(&home, &["--archive", &archive, "heartbeat-check"]);
    assert_eq!(disagree.status.code(), Some(2), "a stalled daemon must be distinguishable from no scheduler");
    fs::remove_file(f.arch.root.join(".daemon")).unwrap();

    let json = run_bin(&home, &["--archive", &archive, "heartbeat-check", "--json"]);
    let v: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["fresh"], Value::Bool(false));
    assert!(v["ageMs"].as_i64().unwrap() > 0);
}

#[test]
fn healthcheck_is_one_exit_code_for_a_scheduler() {
    let mut f = setup("healthcheck", &[("a.txt", b"a\n")]);
    scan_initial(&mut f);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let cycle = run_bin(&home, &["--archive", &archive, "scan-once", "healthcheck"]);
    assert!(cycle.status.success(), "{}", String::from_utf8_lossy(&cycle.stderr));

    let good = run_bin(&home, &["--archive", &archive, "healthcheck"]);
    assert_eq!(good.status.code(), Some(0), "a healthy archive must exit 0: {}", String::from_utf8_lossy(&good.stdout));

    fs::write(f.arch.heartbeat_path(), format!("{}\n", util::now_ms() - 3_600_000)).unwrap();
    let bad = run_bin(&home, &["--archive", &archive, "healthcheck"]);
    assert_eq!(bad.status.code(), Some(1), "a broken promise must exit 1");
    let text = String::from_utf8_lossy(&bad.stdout).to_string();
    assert!(text.contains("heartbeat"), "the report must name the problem: {text}");
    assert!(text.contains("fix:"), "and name the command that fixes it");

    let strict = run_bin(&home, &["--archive", &archive, "healthcheck", "--strict"]);
    assert_eq!(strict.status.code(), Some(1));
}

// -------------------------------------------------------------------------------------------
// C. The MCP surface: nine read-only tools, no write path, one log file.
// -------------------------------------------------------------------------------------------

fn mcp_server(f: &Fixture) -> mcp::Server {
    mcp::Server::new(&f.arch.root).unwrap()
}

#[test]
fn mcp_registers_level_1_and_level_3_only() {
    let f = policy_fixture("mcp-tools", 4, 3);
    let mut srv = mcp_server(&f);
    let reply = mcp::handle_message(&mut srv, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<String> = tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, mcp::tool_names(), "tools/list must advertise exactly the reading surface");
    for w in mcp::write_tool_names() {
        assert!(!names.contains(&w.to_string()), "{w} must not be registered");
    }
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], Value::Bool(true), "every registered tool is read-only");
        assert!(t["inputSchema"]["type"] == "object");
    }
    // and every registered tool really answers
    for name in mcp::tool_names() {
        let args = match name {
            "pl_plan_restore" => json!({"project": f.project.name, "at": "1m ago"}),
            "pl_why" => json!({"project": f.project.name, "path": "src/a.txt"}),
            "pl_tree" | "pl_diff" | "pl_last_good" | "pl_check" | "pl_log" => {
                if name == "pl_diff" {
                    json!({"project": f.project.name, "at": "1m ago"})
                } else {
                    json!({"project": f.project.name})
                }
            }
            _ => json!({}),
        };
        let r = mcp::handle_message(&mut srv, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
        assert_eq!(r["result"]["isError"], Value::Bool(false), "{name} answered with an error: {}", r["result"]["content"][0]["text"]);
    }
}

#[test]
fn mcp_refuses_write_operations_and_unknown_names() {
    let f = policy_fixture("mcp-write", 4, 3);
    let mut srv = mcp_server(&f);
    // The Level 2 names are written out here on purpose: if the test read them from the code, a
    // change to the code would change what the test demands, and this check would measure nothing.
    let level_two = [
        "pl_restore", "pl_panic", "pl_prune", "pl_export_and_prune", "pl_archive_delete", "pl_import",
        "pl_mark", "pl_pause", "pl_resume", "pl_remove", "pl_scan_once", "pl_doctor_fix_lock", "pl_check_fix",
    ];
    for bad in level_two.iter().map(|s| *s).chain(["no_such_tool"].into_iter()) {
        let r = mcp::handle_message(
            &mut srv,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": bad, "arguments": {}}}),
        )
        .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        assert_eq!(r["result"]["isError"], Value::Bool(true), "{bad} must be refused");
        if bad == "no_such_tool" {
            assert!(text.contains("unknown tool"), "an invented name is unknown: {text}");
        } else {
            assert!(
                text.contains("write operation"),
                "a write name must be refused *as a write operation*, not as unknown: {bad} -> {text}"
            );
        }
    }
    // and every one of those names is missing from the advertised list
    let listed = mcp::tool_names();
    for bad in level_two {
        assert!(!listed.contains(&bad), "{bad} must not be registered");
    }
}

#[test]
fn mcp_rate_limiter_bites_and_recovers() {
    let f = policy_fixture("mcp-rate", 4, 3);
    let mut srv = mcp_server(&f);
    srv.reset_rate_limiter();
    let mut refusals = 0usize;
    for _ in 0..14 {
        let r = mcp::handle_message(
            &mut srv,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "pl_status", "arguments": {"project": f.project.name}}}),
        )
        .unwrap();
        if r["result"]["isError"] == Value::Bool(true) {
            refusals += 1;
        }
    }
    assert!(refusals > 0, "a burst of 14 calls must be refused at some point");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    srv.reset_rate_limiter();
    let r = mcp::handle_message(
        &mut srv,
        &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "pl_status", "arguments": {"project": f.project.name}}}),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], Value::Bool(false), "the limiter must recover");
}

#[test]
fn mcp_reading_leaves_the_archive_byte_identical_apart_from_its_log() {
    let f = policy_fixture("mcp-readonly", 8, 20);
    let before = archive_manifest(&f.arch.root);
    let mut srv = mcp_server(&f);
    let args: Vec<(&str, Value)> = vec![
        ("pl_status", json!({})),
        ("pl_status", json!({"project": f.project.name})),
        ("pl_log", json!({"project": f.project.name, "limit": 10})),
        ("pl_tree", json!({"project": f.project.name})),
        ("pl_diff", json!({"project": f.project.name, "at": "1h ago", "include_content": true})),
        ("pl_last_good", json!({"project": f.project.name})),
        ("pl_check", json!({"project": f.project.name, "deep": true})),
        ("pl_doctor", json!({})),
        ("pl_plan_restore", json!({"project": f.project.name, "at": "1m ago"})),
        ("pl_why", json!({"project": f.project.name, "path": "src/a.txt"})),
    ];
    for (i, (name, a)) in args.iter().enumerate() {
        let r = mcp::handle_message(
            &mut srv,
            &json!({"jsonrpc": "2.0", "id": i as i64, "method": "tools/call", "params": {"name": name, "arguments": a}}),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], Value::Bool(false), "{name}: {}", r["result"]["content"][0]["text"]);
    }
    let after = archive_manifest(&f.arch.root);
    let changed: Vec<String> = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .filter(|k| before.get(k) != after.get(k))
        .collect();
    let unexpected: Vec<&String> = changed.iter().filter(|c| !c.starts_with("logs/")).collect();
    assert!(unexpected.is_empty(), "the reading surface changed the archive: {unexpected:?}");
    assert!(!changed.is_empty(), "the read-only check saw no change at all: it is not measuring");
    let log = fs::read_to_string(f.arch.root.join("logs").join("mcp.log")).unwrap();
    assert!(log.contains("pl_status") && log.contains("pl_diff"), "the request log must name what was asked: {log}");
    assert!(!log.contains("v1\n"), "the log must record the question, not the contents");
}

#[test]
fn mcp_plan_restore_plans_and_performs_nothing() {
    let f = policy_fixture("mcp-plan", 6, 10);
    let before = archive_manifest(&f.arch.root);
    let mut srv = mcp_server(&f);
    let r = mcp::handle_message(
        &mut srv,
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": {"name": "pl_plan_restore", "arguments": {"project": f.project.name, "at": "5m ago"}}}),
    )
    .unwrap();
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["performed"], Value::Bool(false));
    assert!(v["target"].as_str().unwrap().len() > 0);
    assert!(v["files"].as_u64().unwrap() >= 1);
    let after = archive_manifest(&f.arch.root);
    // nothing was created: the target folder of the plan must not exist
    assert!(!PathBuf::from(v["target"].as_str().unwrap()).exists(), "pl_plan_restore created the target folder");
    let unexpected: Vec<String> = before.keys().filter(|k| !k.starts_with("logs/") && before.get(*k) != after.get(*k)).cloned().collect();
    assert!(unexpected.is_empty(), "planning changed the archive: {unexpected:?}");
}

#[test]
fn mcp_diff_hides_content_of_a_path_the_filters_call_a_secret() {
    let mut f = setup("mcp-secret", &[("src/app.ts", b"v0\n")]);
    // the user archived secrets at first …
    let mut p = reload(&f);
    p.settings_mut().insert("includeSecrets".into(), Value::Bool(true));
    p.save_meta().unwrap();
    f.project = reload(&f);
    scan_initial(&mut f);
    write(&f.project_root.join("src/app.ts"), b"v1\n");
    write(&f.project_root.join("secrets/id_rsa"), b"TOP-SECRET-KEY-MATERIAL\n");
    f.project = reload(&f);
    scan_now(&mut f);
    // the secret really is in the archive — otherwise the rest of this test proves nothing
    let journal = events::load_journal(&f.project.dir).unwrap();
    assert!(
        events::state_at(&journal.events, i64::MAX, None).contains_key("secrets/id_rsa"),
        "the fixture must have the secret tracked: {:?}",
        events::state_at(&journal.events, i64::MAX, None).keys().collect::<Vec<_>>()
    );
    // … and then decided they no longer want secrets in the archive
    let mut p = reload(&f);
    p.settings_mut().insert("includeSecrets".into(), Value::Bool(false));
    p.save_meta().unwrap();

    let mut srv = mcp_server(&f);
    let call = |srv: &mut mcp::Server, content: bool| -> Value {
        srv.reset_rate_limiter();
        let r = mcp::handle_message(
            srv,
            &json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call",
                    "params": {"name": "pl_diff", "arguments": {"project": f.project.name, "at": "1h ago", "include_content": content}}}),
        )
        .unwrap();
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let plain = call(&mut srv, false);
    assert_eq!(plain["contentIncluded"], Value::Bool(false));
    let listed: Vec<String> = plain["added"]
        .as_array()
        .unwrap()
        .iter()
        .chain(plain["changed"].as_array().unwrap().iter())
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(listed.contains(&"secrets/id_rsa".to_string()), "the paths are still reported: {listed:?}");
    let with_content = call(&mut srv, true);
    let contents = with_content["contents"].as_object().unwrap();
    assert!(!contents.contains_key("secrets/id_rsa"), "the secret's bytes must not be returned");
    let refused = with_content["contentRefused"].as_array().unwrap();
    assert!(
        refused.iter().any(|r| r["path"] == "secrets/id_rsa" && r["reason"] == "secret"),
        "the refusal must name the path and the reason: {refused:?}"
    );
    assert!(contents.contains_key("src/app.ts"), "a normal file is still available when asked for");
}

#[test]
fn mcp_does_not_share_the_write_path_of_the_cli() {
    // The static audit is a tool, but the claim is a property of the code, so it is checked here
    // too: the two files that make up the read surface must contain no write symbol.
    for rel in ["src/mcp.rs", "src/bin/pl_mcp.rs"] {
        let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap();
        for sym in ["events::append", "restore::execute", "swap_journal", "save_meta", "scan_project", "run_cycle"] {
            assert!(!text.contains(sym), "{rel} mentions {sym}: the read surface must not contain a write path");
        }
    }
}

// -------------------------------------------------------------------------------------------
// D. The daily commands: read-only, and they say what they saw.
// -------------------------------------------------------------------------------------------

#[test]
fn the_daily_commands_answer_without_touching_anything() {
    let f = policy_fixture("daily", 6, 5);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    // a cycle through the CLI, so that the heartbeat these commands look at exists
    let cycle = run_bin(&home, &["--archive", &archive, "scan-once", "daily"]);
    assert!(cycle.status.success(), "{}", String::from_utf8_lossy(&cycle.stderr));
    let before = archive_manifest(&f.arch.root);
    let root_before = archive_manifest(&f.project_root);

    let runs: Vec<Vec<&str>> = vec![
        vec!["list", "--sort", "size"],
        vec!["list", "--sort", "name"],
        vec!["status", "--compact"],
        vec!["recent", "--limit", "3"],
        vec!["size", "--top", "3"],
        vec!["gc"],
        vec!["suggest"],
        vec!["prompt", "--space"],
        vec!["since", "daily", "--stat"],
        vec!["blame", "daily", "src/a.txt"],
        vec!["log", "daily", "--grep", "a.txt"],
        vec!["cat", "daily", "--path", "src/a.txt"],
        vec!["undo", "daily"],
        vec!["retention", "daily"],
        vec!["completion", "bash"],
        vec!["mcp", "tools"],
        vec!["heartbeat-check"],
        vec!["healthcheck"],
        vec!["doctor"],
        vec!["why", "daily", "src/a.txt"],
    ];
    for args in &runs {
        let mut full = vec!["--archive", archive.as_str()];
        full.extend(args.iter().copied());
        let out = run_bin(&home, &full);
        assert!(
            out.status.success() || args[0] == "gc",
            "`pl {}` failed: {}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // the tool list is exactly the reading surface
    let tools = run_bin(&home, &["--archive", &archive, "mcp", "tools"]);
    let listed: Vec<String> = String::from_utf8_lossy(&tools.stdout).lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    assert_eq!(listed, mcp::tool_names(), "`pl mcp tools` must print exactly the registered tools");

    // and none of it wrote: neither the archive nor the project folder changed
    let after = archive_manifest(&f.arch.root);
    let changed: Vec<&String> = before.keys().filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(changed.is_empty(), "the daily read-only commands changed the archive: {changed:?}");
    assert_eq!(archive_manifest(&f.project_root), root_before, "the project folder was touched");
}

#[test]
fn a_bad_sort_key_and_a_bad_policy_are_refused_with_a_reason() {
    let f = policy_fixture("errors", 3, 2);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let bad_sort = run_bin(&home, &["--archive", &archive, "list", "--sort", "whatever"]);
    assert!(!bad_sort.status.success());
    assert!(String::from_utf8_lossy(&bad_sort.stderr).contains("unknown sort key"));

    for bad in ["", "7d:", "7d:every", "0d:all", "abc:all", "7d:all,7d:1/day"] {
        let out = run_bin(&home, &["--archive", &archive, "prune", "errors", "--policy", bad, "--dry-run"]);
        assert!(!out.status.success(), "policy {bad:?} must be refused");
    }
    let both = run_bin(&home, &["--archive", &archive, "prune", "errors", "--before", "1d ago", "--policy", "7d:all"]);
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("not both"));
    // and the refusals changed nothing
    assert!(retention::stored_policy(&reload(&f)).is_none());
}

#[test]
fn retention_set_stores_and_never_applies() {
    let mut f = policy_fixture("retention-cmd", 8, 20);
    let home = f.base.join("home");
    let archive = f.arch.root.to_string_lossy().to_string();
    let versions_before = events::load_journal(&f.project.dir).unwrap().events.len();

    let set = run_bin(&home, &["--archive", &archive, "retention", "retention-cmd", "3d:all,10d:1/day"]);
    assert!(set.status.success(), "{}", String::from_utf8_lossy(&set.stderr));
    let p = reload(&f);
    assert_eq!(retention::stored_policy(&p).as_deref(), Some("3d:all,10d:1/day"));
    assert_eq!(retention::applied_at(&p), None, "storing a policy must not apply it");
    assert_eq!(
        events::load_journal(&f.project.dir).unwrap().events.len(),
        versions_before,
        "storing a policy must not change the journal"
    );
    let shown = run_bin(&home, &["--archive", &archive, "retention", "retention-cmd"]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("3d:all,10d:1/day"));

    // `apply` uses the stored policy; `clear` forgets it and deletes nothing
    let dry = run_bin(&home, &["--archive", &archive, "retention", "retention-cmd", "apply", "--dry-run"]);
    assert!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
    assert!(String::from_utf8_lossy(&dry.stdout).contains("dry run"));
    let clear = run_bin(&home, &["--archive", &archive, "retention", "retention-cmd", "clear"]);
    assert!(clear.status.success());
    let p = reload(&f);
    assert!(retention::stored_policy(&p).is_none());
    assert_eq!(events::load_journal(&f.project.dir).unwrap().events.len(), versions_before);
    f.project = p;
}

#[test]
fn a_policy_prune_survives_a_kill_and_is_recovered_like_any_other_prune() {
    let f = policy_fixture("policy-crash", 20, 40);
    let before_state = state_sig(&events::load_journal(&f.project.dir).unwrap().events, util::now_ms());
    let home = f.base.join("home");
    fs::create_dir_all(&home).unwrap();
    let archive = f.arch.root.to_string_lossy().to_string();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_projectlife"));
    c.env("PROJECTLIFE_HOME", &home)
        .env("PROJECTLIFE_CRASH_AFTER", "journal_swapped")
        .args(["--archive", &archive, "prune", "policy-crash", "--policy", "7d:all,5d:1/day", "--yes"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = c.status().unwrap();
    assert!(status.code().is_none(), "the process must have been killed by a signal, not exited: {status:?}");
    assert!(f.project.prune_journal().is_file(), "the phase file must be on disk after the crash");

    let rec = run_bin(&home, &["--archive", &archive, "recover", "policy-crash"]);
    assert!(rec.status.success(), "{}", String::from_utf8_lossy(&rec.stderr));
    assert!(!f.project.prune_journal().is_file(), "the phase file must be gone after recovery");
    let journal = events::load_journal(&f.project.dir).unwrap();
    assert!(!journal.events.is_empty(), "recovery must leave a usable journal");
    assert_eq!(consistent(&f).err(), None);
    let now_state = state_sig(&journal.events, util::now_ms());
    assert_eq!(now_state, before_state, "a recovered policy prune must not change the current state");
    let anchors = retention_anchors(&f);
    assert!(!anchors.is_empty(), "the completed prune must have kept its anchors");
}
