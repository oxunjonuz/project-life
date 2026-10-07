//! The MCP server: a read-only window onto an archive for an agent.
//!
//! Three levels were specified; only two of them exist in this binary.
//!
//! * **Level 1 — reading.** Registered: `pl_status`, `pl_log`, `pl_why`, `pl_tree`, `pl_diff`,
//!   `pl_last_good`, `pl_check`, `pl_doctor`.
//! * **Level 2 — writing.** NOT registered and not reachable: `restore`, `panic`, `prune`,
//!   `export-and-prune`, `archive-delete`, `import`, `mark`, `pause`/`resume`/`remove`, `scan-once`,
//!   `doctor --fix-lock`, `check --fix`. These names are not in `tools/list`, so an agent cannot
//!   see them, and a call to one is refused as an unknown tool.
//! * **Level 3 — proposing.** `pl_plan_restore` returns a plan in JSON and performs nothing.
//!
//! The boundary is enforced by construction, not by intention: every tool here is implemented on
//! top of `quick` (read-only by module contract), `events::state_at`, `restore::plan`,
//! `doctor::doctor` and the verified store reader. There is no call to any function that writes
//! journal, blob or metadata; `tools/mcp_audit.py` checks exactly that against a list of write
//! symbols, and the acceptance test asserts that a full exercise of every tool leaves the archive
//! byte-identical apart from `logs/mcp.log`.
//!
//! The single write this process performs is the request log (`logs/mcp.log`): who asked, what
//! they asked, what came back. Without it "who read my archive" has no answer.

use crate::archive::{iso_ms, Archive, Project};
use crate::events;
use crate::quick;
use crate::restore::{self, RestoreOptions};
use crate::retention;
use crate::util;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "projectlife-mcp";
/// Ten calls per second per tool, as specified.
const RATE_LIMIT_PER_SECOND: usize = 10;
/// A single tool result is capped so that one call cannot stream a whole project through an agent.
const MAX_CONTENT_BYTES: u64 = 256 * 1024;
const MAX_TREE_ENTRIES: usize = 5000;

/// Level 2 — the names that exist in the CLI and are deliberately absent here.
pub fn write_tool_names() -> Vec<&'static str> {
    vec![
        "pl_restore",
        "pl_panic",
        "pl_prune",
        "pl_export_and_prune",
        "pl_archive_delete",
        "pl_import",
        "pl_mark",
        "pl_pause",
        "pl_resume",
        "pl_remove",
        "pl_scan_once",
        "pl_doctor_fix_lock",
        "pl_check_fix",
    ]
}

/// Level 1 + Level 3, as they are advertised in `tools/list`.
pub fn tool_names() -> Vec<&'static str> {
    vec![
        "pl_status",
        "pl_log",
        "pl_why",
        "pl_tree",
        "pl_diff",
        "pl_last_good",
        "pl_check",
        "pl_doctor",
        "pl_plan_restore",
    ]
}

fn schema(props: Value, required: Vec<&str>) -> Value {
    let mut req: Vec<Value> = required.into_iter().map(|s| Value::from(s)).collect();
    req.sort_by_key(|v| v.as_str().unwrap_or("").to_string());
    json!({"type": "object", "properties": props, "required": req, "additionalProperties": false})
}

pub fn tool_defs() -> Vec<Value> {
    let s = |d: &str| json!({"type": "string", "description": d});
    let b = |d: &str| json!({"type": "boolean", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    vec![
        json!({
            "name": "pl_status",
            "description": "Read-only status of one project or of every project in the archive: state, version count, last observed moment, size, pending recovery. Never writes.",
            "inputSchema": schema(json!({"project": s("project name or id; omit for all projects")}), vec![]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_log",
            "description": "Read-only journal of observed events (put, delete, move, skip, mass, gap, mark). Filter by moment, type, path or a case-insensitive substring.",
            "inputSchema": schema(json!({
                "project": s("project name or id"),
                "since": s("moment, e.g. \"2h ago\" or an ISO timestamp"),
                "until": s("moment"),
                "type": s("comma-separated event types, e.g. mass,gap"),
                "path": s("limit to one path or folder"),
                "grep": s("case-insensitive substring in the event"),
                "limit": n("how many events, newest first (default 100, max 1000)")
            }), vec!["project"]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_why",
            "description": "Read-only answer to \"why is this file not in the archive?\" — the filter decision and the recorded skip, with the rule that caused it.",
            "inputSchema": schema(json!({
                "project": s("project name or id"),
                "path": s("path relative to the project root")
            }), vec!["project", "path"]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_tree",
            "description": "Read-only file list of the project at one moment, with sizes and content hashes. No bytes are returned.",
            "inputSchema": schema(json!({
                "project": s("project name or id"),
                "at": s("moment; default: the last observed moment")
            }), vec!["project"]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_diff",
            "description": "Read-only comparison of two moments. By default only the paths are returned (added/removed/changed). Content requires include_content=true and is refused for any path the filters treat as a secret.",
            "inputSchema": schema(json!({
                "project": s("project name or id"),
                "at": s("moment to compare from"),
                "to": s("moment to compare to; default: the last observed moment"),
                "path": s("limit to one path or folder"),
                "include_content": b("include the file contents of changed files (default false)")
            }), vec!["project", "at"]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_last_good",
            "description": "Read-only: the newest mass event and the moment just before it, with the command a human would run to restore it.",
            "inputSchema": schema(json!({"project": s("project name or id")}), vec!["project"]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_check",
            "description": "Read-only verification: every blob the journal references is present, and (with deep=true) re-read and matched against its name. Reports; never fixes.",
            "inputSchema": schema(json!({
                "project": s("project name or id; omit for all"),
                "deep": b("re-read every blob and verify its sha256 (default false)")
            }), vec![]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_doctor",
            "description": "Read-only doctor report: free space, heartbeat, locks, project states, pending recoveries. Never fixes.",
            "inputSchema": schema(json!({}), vec![]),
            "annotations": {"readOnlyHint": true, "level": 1}
        }),
        json!({
            "name": "pl_plan_restore",
            "description": "Level 3: returns what a restore WOULD do (target folder, counts, missing blobs) as JSON and performs nothing. A human runs the printed command.",
            "inputSchema": schema(json!({
                "project": s("project name or id"),
                "at": s("moment to restore"),
                "last_good": b("use the last good moment instead of at"),
                "mark": s("use the moment of a mark instead of at"),
                "path": s("limit to one path or folder"),
                "to": s("target folder (not created; defaults to <project>-restored-<stamp>)")
            }), vec!["project"]),
            "annotations": {"readOnlyHint": true, "level": 3}
        }),
    ]
}

/// Rate limiter: per tool, at most `RATE_LIMIT_PER_SECOND` calls in any one-second window.
struct Limiter {
    hits: HashMap<String, Vec<i64>>,
}

impl Limiter {
    fn new() -> Self {
        Limiter { hits: HashMap::new() }
    }
    fn allow(&mut self, tool: &str, now: i64) -> bool {
        let v = self.hits.entry(tool.to_string()).or_default();
        v.retain(|t| now - *t < 1000);
        if v.len() >= RATE_LIMIT_PER_SECOND {
            return false;
        }
        v.push(now);
        true
    }
}

pub struct Server {
    pub archive: Archive,
    pub client: String,
    limiter: Limiter,
    pub calls: usize,
    pub refusals: usize,
    log_to_file: bool,
}

impl Server {
    pub fn new(root: &Path) -> Result<Server, String> {
        let archive = Archive::open(root)?;
        Ok(Server { archive, client: "unknown".into(), limiter: Limiter::new(), calls: 0, refusals: 0, log_to_file: true })
    }

    /// Start the rate limiter from scratch. The tests need this: without it they inherit the calls
    /// of the assertions before them, in the same second.
    pub fn reset_rate_limiter(&mut self) {
        self.limiter = Limiter::new();
    }

    /// The one write this process makes. It goes to `logs/mcp.log`, never into a project.
    fn log(&self, tool: &str, args: &Value, outcome: &str, ms: i64) {
        self.log_text(&format!("tool={tool} args={} -> {outcome} ({ms} ms)", redact_args(args)));
    }

    /// A free-form line in the same file: the server's own start and exit, and nothing else.
    fn log_text(&self, text: &str) {
        if !self.log_to_file {
            return;
        }
        let dir = self.archive.root.join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let line = format!("{} [{}] mcp client={} {}\n", util::fmt_local(util::now_ms()), std::process::id(), self.client, text);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("mcp.log")) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Parameters are logged as keys only: the log says *what was asked*, never contents.
fn redact_args(args: &Value) -> String {
    match args.as_object() {
        Some(m) if !m.is_empty() => {
            let mut keys: Vec<String> = m.keys().cloned().collect();
            keys.sort();
            let mut out = String::from("{");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let v = &m[k];
                let shown = match v {
                    Value::String(s) => format!("\"{}\"", s.replace('"', "'")),
                    other => other.to_string(),
                };
                out.push_str(&format!("{k}={shown}"));
            }
            out.push('}');
            out
        }
        _ => "{}".into(),
    }
}

fn moment(spec: &str, now: i64) -> Result<i64, String> {
    util::parse_at(spec, now)
}

fn project_of(srv: &Server, args: &Value) -> Result<Project, String> {
    let name = args
        .get("project")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "the \"project\" argument is required".to_string())?;
    srv.archive.find(name)
}

fn state_at_or_last(project: &Project, spec: Option<&str>) -> Result<(i64, String), String> {
    let journal = events::load_journal(&project.dir)?;
    match spec {
        Some(s) => Ok((moment(s, util::now_ms())?, s.to_string())),
        None => {
            let t = events::last_observed_at(&journal.events)
                .ok_or_else(|| format!("{}: nothing has been observed yet", project.name))?;
            Ok((t, "last observed".into()))
        }
    }
}

/// The tool surface. Every branch below is read-only; see the module comment for how that is
/// checked rather than asserted.
pub fn call_tool(srv: &mut Server, name: &str, args: &Value) -> Result<Value, String> {
    let now = util::now_ms();
    if write_tool_names().contains(&name) {
        return Err(format!(
            "tool \"{name}\" is a write operation and is not registered with this server: {name} cannot be called here"
        ));
    }
    if !tool_names().contains(&name) {
        return Err(format!("unknown tool: {name}"));
    }
    if !srv.limiter.allow(name, now) {
        srv.refusals += 1;
        srv.log(name, args, "rate limited", 0);
        return Err(format!("rate limit: at most {RATE_LIMIT_PER_SECOND} calls per second per tool"));
    }
    srv.calls += 1;
    let started = util::now_ms();
    let out = (|| -> Result<Value, String> {
        match name {
            "pl_status" => {
                let rows = quick::status_rows(&srv.archive)?;
                let want = args.get("project").and_then(|v| v.as_str());
                let items: Vec<Value> = rows
                    .iter()
                    .filter(|r| want.map(|w| r.name == w).unwrap_or(true))
                    .map(|r| r.to_json())
                    .collect();
                if items.is_empty() {
                    return Err(match want {
                        Some(w) => format!("project not found: {w}"),
                        None => "the archive has no projects".into(),
                    });
                }
                Ok(json!({"archive": srv.archive.root.to_string_lossy(), "projects": items}))
            }
            "pl_log" => {
                let p = project_of(srv, args)?;
                let journal = events::load_journal(&p.dir)?;
                let since = match args.get("since").and_then(|v| v.as_str()) {
                    Some(s) => Some(moment(s, now)?),
                    None => None,
                };
                let until = match args.get("until").and_then(|v| v.as_str()) {
                    Some(s) => Some(moment(s, now)?),
                    None => None,
                };
                let types: Vec<String> = args
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(|t| t.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                let path = args.get("path").and_then(|v| v.as_str()).map(util::norm_rel);
                let grep = args.get("grep").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100).min(1000) as usize;
                let mut rows: Vec<&events::Ev> = Vec::new();
                for e in journal.events.iter().rev() {
                    let ts = events::ts_of(e);
                    if since.map(|s| ts < s).unwrap_or(false) || until.map(|u| ts > u).unwrap_or(false) {
                        continue;
                    }
                    if !types.is_empty() && !types.iter().any(|t| t == events::event_type(e)) {
                        continue;
                    }
                    if let Some(pf) = &path {
                        let hit = [events::get_str(e, "path"), events::get_str(e, "to"), events::get_str(e, "from")]
                            .into_iter()
                            .flatten()
                            .any(|x| x == *pf || x.starts_with(&format!("{pf}/")));
                        if !hit {
                            continue;
                        }
                    }
                    if let Some(g) = &grep {
                        let line = serde_json::to_string(&Value::Object(e.clone())).unwrap_or_default().to_lowercase();
                        if !line.contains(g) {
                            continue;
                        }
                    }
                    rows.push(e);
                    if rows.len() >= limit {
                        break;
                    }
                }
                Ok(json!({
                    "project": p.name,
                    "events": rows.iter().map(|e| Value::Object((*e).clone())).collect::<Vec<_>>(),
                    "returned": rows.len(),
                    "limit": limit,
                }))
            }
            "pl_why" => {
                let p = project_of(srv, args)?;
                let rel = util::norm_rel(args.get("path").and_then(|v| v.as_str()).unwrap_or(""));
                if rel.is_empty() {
                    return Err("the \"path\" argument is required".into());
                }
                let journal = events::load_journal(&p.dir)?;
                let last_skip = journal
                    .events
                    .iter()
                    .rev()
                    .find(|e| {
                        events::event_type(e) == "skip" && events::get_str(e, "path").as_deref() == Some(rel.as_str())
                    })
                    .map(|e| Value::Object(e.clone()));
                let filter = crate::filters::FilterConfig::for_profile(&p.profile(), &p.settings());
                let (own, git) = crate::scan::open_ignore_rules(&p.project_path());
                let decision = filter.decide_file(&rel, 0, &own, &git);
                let tracked = quick::last_observed(&p)?
                    .map(|t| events::state_at(&journal.events, t, None).contains_key(&rel))
                    .unwrap_or(false);
                Ok(json!({
                    "project": p.name, "path": rel, "trackedNow": tracked,
                    "decision": {"track": decision.track, "reason": decision.reason, "rule": decision.rule},
                    "lastSkip": last_skip,
                    "note": "the decision is computed with the project's own filter settings; reason=none means nothing in the filters excludes it",
                }))
            }
            "pl_tree" => {
                let p = project_of(srv, args)?;
                let at_arg = args.get("at").and_then(|v| v.as_str());
                let (at, label) = state_at_or_last(&p, at_arg)?;
                let journal = events::load_journal(&p.dir)?;
                let state = events::state_at(&journal.events, at, None);
                let mut files: Vec<Value> = Vec::new();
                for (rel, f) in state.iter() {
                    if files.len() >= MAX_TREE_ENTRIES {
                        break;
                    }
                    files.push(json!({
                        "path": rel, "kind": f.kind, "size": f.size,
                        "sha256": if f.hash.is_empty() { Value::Null } else { Value::from(f.hash.clone()) },
                        "target": f.target,
                    }));
                }
                Ok(json!({
                    "project": p.name, "at": iso_ms(at), "atLabel": label,
                    "files": files, "count": state.len(), "truncated": state.len() > MAX_TREE_ENTRIES,
                }))
            }
            "pl_diff" => {
                let p = project_of(srv, args)?;
                let at_spec = args
                    .get("at")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "the \"at\" argument is required".to_string())?;
                let from = moment(at_spec, now)?;
                let (to, to_label) = state_at_or_last(&p, args.get("to").and_then(|v| v.as_str()))?;
                let journal = events::load_journal(&p.dir)?;
                let a = events::state_at(&journal.events, from, None);
                let b = events::state_at(&journal.events, to, None);
                let filter = args.get("path").and_then(|v| v.as_str()).map(util::norm_rel);
                let d = quick::diff_states(&a, &b, filter.as_deref());
                let mut out = json!({
                    "project": p.name,
                    "from": iso_ms(from), "to": iso_ms(to), "toLabel": to_label,
                    "added": d.added, "removed": d.removed, "changed": d.changed,
                    "contentIncluded": false,
                });
                if args.get("include_content").and_then(|v| v.as_bool()).unwrap_or(false) {
                    // Contents are read only through the filters: a path the project treats as a
                    // secret is refused here even when the agent asks for it by name.
                    let fcfg = crate::filters::FilterConfig::for_profile(&p.profile(), &p.settings());
                    let (own, git) = crate::scan::open_ignore_rules(&p.project_path());
                    let store = crate::store::Store::new(&p.blobs_dir(), &p.tmp_dir());
                    let mut contents = serde_json::Map::new();
                    let mut refused: Vec<Value> = Vec::new();
                    for rel in d.changed.iter().chain(d.added.iter()) {
                        let size = b.get(rel).map(|f| f.size).unwrap_or(0);
                        let dec = fcfg.decide_file(rel, size, &own, &git);
                        if !dec.track {
                            refused.push(json!({"path": rel, "reason": dec.reason, "rule": dec.rule}));
                            continue;
                        }
                        if size > MAX_CONTENT_BYTES {
                            refused.push(json!({"path": rel, "reason": "content_too_large", "rule": format!("> {MAX_CONTENT_BYTES} B")}));
                            continue;
                        }
                        let hash = b.get(rel).map(|f| f.hash.clone()).unwrap_or_default();
                        match store.read_verified(&hash) {
                            Ok(data) => {
                                contents.insert(rel.clone(), Value::from(String::from_utf8_lossy(&data).to_string()));
                            }
                            Err(e) => {
                                refused.push(json!({"path": rel, "reason": "unreadable", "rule": e}));
                            }
                        }
                    }
                    out["contentIncluded"] = Value::from(true);
                    out["contents"] = Value::Object(contents);
                    out["contentRefused"] = Value::Array(refused);
                }
                Ok(out)
            }
            "pl_last_good" => {
                let p = project_of(srv, args)?;
                match quick::last_good_of(&p)? {
                    Some((ts, why)) => Ok(json!({
                        "project": p.name, "lastGood": iso_ms(ts), "reason": why,
                        "command": format!("pl restore {} --last-good --to ../{}-recovered", p.name, p.name),
                    })),
                    None => Ok(json!({
                        "project": p.name, "lastGood": Value::Null,
                        "reason": "no mass event in this history",
                        "command": format!("pl restore {} --at \"10m ago\" --to ../{}-recovered", p.name, p.name),
                    })),
                }
            }
            "pl_check" => {
                let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
                let projects: Vec<Project> = match args.get("project").and_then(|v| v.as_str()) {
                    Some(name) => vec![srv.archive.find(name)?],
                    None => srv.archive.load_projects()?,
                };
                let mut results: Vec<Value> = Vec::new();
                for p in projects {
                    let journal = match events::load_journal(&p.dir) {
                        Ok(j) => j,
                        Err(e) => {
                            results.push(json!({"project": p.name, "journalReadable": false, "error": e}));
                            continue;
                        }
                    };
                    let store = crate::store::Store::new(&p.blobs_dir(), &p.tmp_dir());
                    let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                    for e in &journal.events {
                        if events::event_type(e) == "put" {
                            if let Some(h) = events::get_str(e, "hash") {
                                referenced.insert(h);
                            }
                        }
                    }
                    let missing: Vec<String> = referenced.iter().filter(|h| !store.has(h)).cloned().collect();
                    let mut corrupted: Vec<String> = Vec::new();
                    if deep {
                        for h in referenced.iter() {
                            if let Err(_e) = store.read_verified(h) {
                                corrupted.push(h.clone());
                            }
                        }
                    }
                    results.push(json!({
                        "project": p.name,
                        "journalReadable": true,
                        "versions": referenced.len(),
                        "missingBlobs": missing,
                        "corruptedBlobs": corrupted,
                        "deep": deep,
                        "ok": missing.is_empty() && corrupted.is_empty(),
                    }));
                }
                Ok(json!({"checked": results}))
            }
            "pl_doctor" => {
                let checks = crate::doctor::doctor(&srv.archive);
                Ok(json!({
                    "checks": checks.iter().map(|c| json!({"level": c.level, "text": c.text, "fix": c.fix})).collect::<Vec<_>>(),
                    "errors": checks.iter().filter(|c| c.level == crate::doctor::ERROR).count(),
                    "warnings": checks.iter().filter(|c| c.level == crate::doctor::WARN).count(),
                }))
            }
            "pl_plan_restore" => {
                let p = project_of(srv, args)?;
                let journal = events::load_journal(&p.dir)?;
                let (at, label) = if args.get("last_good").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let (ts, why) = quick::last_good_of(&p)?
                        .ok_or_else(|| format!("{}: no mass event, there is no \"last good\" point", p.name))?;
                    (ts, format!("last good ({why})"))
                } else if let Some(m) = args.get("mark").and_then(|v| v.as_str()) {
                    let ts = journal
                        .events
                        .iter()
                        .filter(|e| events::event_type(e) == "mark" && events::get_str(e, "label").as_deref() == Some(m))
                        .max_by_key(|e| events::ts_of(e))
                        .map(events::ts_of)
                        .ok_or_else(|| format!("no mark named \"{m}\""))?;
                    (ts, format!("mark \"{m}\""))
                } else {
                    let spec = args
                        .get("at")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "give one of: at, last_good, mark".to_string())?;
                    (moment(spec, now)?, spec.to_string())
                };
                let paths: Vec<String> = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|s| vec![util::norm_rel(s)])
                    .unwrap_or_default();
                let opts = RestoreOptions {
                    at,
                    at_label: label.clone(),
                    paths: paths.clone(),
                    to: args.get("to").and_then(|v| v.as_str()).map(PathBuf::from),
                    into_project: false,
                    clean: false,
                    preview: true,
                    missing: false,
                };
                let plan = restore::plan(&srv.archive, &p, &opts)?;
                Ok(json!({
                    "project": p.name,
                    "at": iso_ms(at), "atLabel": label,
                    "target": plan.target.to_string_lossy(),
                    "create": plan.create, "overwrite": plan.overwrite,
                    "deleteExtra": if opts.into_project { plan.delete_extra } else { 0 },
                    "files": plan.total(), "bytes": plan.bytes,
                    "missingBlobs": plan.missing_blobs,
                    "warnings": plan.warnings,
                    "thinnedByPolicy": retention::thinned_at(&p).ok().flatten().map(iso_ms),
                    "performed": false,
                    "command": format!("pl restore {} --at \"{}\" --to \"{}\"", p.name, label, plan.target.to_string_lossy()),
                    "note": "nothing was created or written; a human runs the command",
                }))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    })();
    let ms = util::now_ms() - started;
    match &out {
        Ok(_) => srv.log(name, args, "ok", ms),
        Err(e) => srv.log(name, args, &format!("error: {e}"), ms),
    }
    out
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// One JSON-RPC message in, one (or none) out. Public so the tests can drive the protocol without
/// spawning a process.
pub fn handle_message(srv: &mut Server, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    if id.is_null() {
        // a notification: no reply, but the client name is worth keeping
        if method == "notifications/initialized" {
            return None;
        }
        return None;
    }
    match method {
        "initialize" => {
            if let Some(name) = msg
                .get("params")
                .and_then(|p| p.get("clientInfo"))
                .and_then(|c| c.get("name"))
                .and_then(|n| n.as_str())
            {
                srv.client = name.to_string();
            } else if let Some(name) = msg.get("params").and_then(|p| p.get("clientInfo")).and_then(|c| c.get("name")) {
                srv.client = name.to_string();
            }
            Some(rpc_result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": SERVER_NAME, "version": crate::VERSION},
                    "instructions": "Read-only access to a Project Life archive. Restores, prunes and every other write are NOT available here: use pl_plan_restore and run the printed command yourself.",
                }),
            ))
        }
        "ping" => Some(rpc_result(id, json!({}))),
        "tools/list" => Some(rpc_result(id, json!({"tools": tool_defs()}))),
        "tools/call" => {
            let name = msg
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = msg.get("params").and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
            match call_tool(srv, &name, &args) {
                Ok(v) => Some(rpc_result(
                    id,
                    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false}),
                )),
                Err(e) => {
                    // A tool-level refusal is a result with isError, not a transport error: the
                    // client must be able to see "you may not do that" as a structured answer.
                    Some(rpc_result(
                        id,
                        json!({"content": [{"type": "text", "text": e}], "isError": true}),
                    ))
                }
            }
        }
        other => Some(rpc_error(id, -32601, &format!("method not found: {other}"))),
    }
}

/// The stdio loop: newline-delimited JSON-RPC, as the MCP stdio transport specifies.
pub fn serve(root: &Path) -> Result<i32, String> {
    let mut srv = Server::new(root)?;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let reply = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
                writeln!(out, "{reply}").map_err(|e| e.to_string())?;
                out.flush().ok();
                continue;
            }
        };
        if let Some(reply) = handle_message(&mut srv, &msg) {
            writeln!(out, "{reply}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    srv.log_text(&format!(
        "server exited: {} tool calls, {} refusals, client={} — this log file is the only thing this process writes",
        srv.calls, srv.refusals, srv.client
    ));
    Ok(0)
}
