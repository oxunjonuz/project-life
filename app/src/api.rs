//! The routes the window talks to.
//!
//! Every one of them either calls the core binary or reads a file the core documents. Nothing here
//! writes into a project or an archive by itself: the app is a shell around the same program the
//! owner uses from the terminal, and the mutation paths are literally its command lines.

use crate::http::{Request, Response};
use crate::jobs::{self, JobHandle, Registry};
use crate::pl::{self, Core};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

pub struct Ctx {
    pub bin: PathBuf,
    pub home: Option<PathBuf>,
    pub archive: Mutex<Option<PathBuf>>,
    pub token: String,
    pub jobs: Arc<Registry>,
    pub daemon: Mutex<Option<Child>>,
    pub daemon_log: PathBuf,
    pub native: bool,
    pub ui_lang: Mutex<String>,
    /// When this app last stopped observation itself. A fresh heartbeat right after that would
    /// otherwise read as "protected by an external timer" — which it is not.
    pub self_stopped_ms: Mutex<Option<i64>>,
}

impl Ctx {
    pub fn core(&self) -> Core {
        Core {
            bin: self.bin.clone(),
            archive: self.archive.lock().ok().and_then(|a| a.clone()),
            home: self.home.clone(),
        }
    }
    pub fn archive_root(&self) -> Option<PathBuf> {
        if let Some(a) = self.archive.lock().ok().and_then(|a| a.clone()) {
            return Some(a);
        }
        let loc = pl::read_location(self.home.as_deref())?;
        if pl::is_archive(&loc) {
            Some(loc)
        } else {
            None
        }
    }
    pub fn set_archive(&self, root: Option<PathBuf>) {
        if let Ok(mut a) = self.archive.lock() {
            *a = root;
        }
    }
}

fn ok(v: Value) -> Response {
    Response::json(&v)
}

fn err(status: u16, msg: &str) -> Response {
    Response::json_err(status, msg)
}

/// The token check. Only the app's own window knows the token; it is generated at launch.
pub fn authorized(ctx: &Ctx, req: &Request) -> bool {
    if let Some(t) = req.param("token") {
        if t == ctx.token {
            return true;
        }
    }
    if let Some(t) = req.headers.get("x-pl-token") {
        if *t == ctx.token {
            return true;
        }
    }
    false
}

pub fn route(ctx: &Arc<Ctx>, req: &Request) -> Response {
    // The window's own files are served without the token: they are the static UI (the same bytes
    // that are compiled into this binary), and the browser asks for them without a query string.
    // Everything that can *do* something — every /api route — requires the token.
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => return Response::asset(crate::assets::INDEX_HTML, "text/html; charset=utf-8"),
        ("GET", "/app.css") => return Response::asset(crate::assets::APP_CSS, "text/css; charset=utf-8"),
        ("GET", "/app.js") => return Response::asset(crate::assets::APP_JS, "application/javascript; charset=utf-8"),
        _ => {}
    }
    if !authorized(ctx, req) {
        return err(403, "missing or wrong token");
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/bootstrap") => bootstrap(ctx),
        ("POST", "/api/archive/init") => archive_init(ctx, req),
        ("POST", "/api/archive/use") => archive_use(ctx, req),
        ("POST", "/api/archive/pick") => archive_pick(ctx, req),
        ("POST", "/api/detect") => detect(ctx, req),
        ("GET", "/api/presets") => presets(ctx),
        ("POST", "/api/add/preview") => add_preview(ctx, req),
        ("POST", "/api/add") => add_start(ctx, req),
        ("GET", "/api/job") => match req.param("id").and_then(|s| s.parse::<u64>().ok()).and_then(|id| ctx.jobs.get(id)) {
            Some(j) => ok(j.to_json()),
            None => err(404, "no such job"),
        },
        ("GET", "/api/jobs") => ok(json!(ctx.jobs.list().iter().map(|j| j.to_json()).collect::<Vec<_>>())),
        ("GET", "/api/project") => project_detail(ctx, req),
        ("GET", "/api/project/tree") => project_tree(ctx, req),
        ("POST", "/api/project/restore") => restore_start(ctx, req),
        ("POST", "/api/project/export") => export_start(ctx, req),
        ("POST", "/api/import") => import_start(ctx, req),
        ("GET", "/api/watch") => watch_state(ctx),
        ("POST", "/api/watch/start") => watch_start(ctx, req),
        ("POST", "/api/watch/stop") => watch_stop(ctx),
        ("GET", "/api/doctor") => doctor(ctx),
        ("GET", "/api/log") => raw_log(ctx),
        ("POST", "/api/config") => config_set(ctx, req),
        ("POST", "/api/pass") => pass_start(ctx),
        ("POST", "/api/project/pause") => pause_project(ctx, req),
        ("POST", "/api/lang") => {
            let lang = req.str_field("lang").unwrap_or_else(|| "en".into());
            if let Ok(mut l) = ctx.ui_lang.lock() {
                *l = lang.clone();
            }
            ok(json!({"lang": lang}))
        }
        ("POST", "/api/shutdown") => {
            stop_daemon(ctx);
            ok(json!({"stopping": true}))
        }
        // ---- round 297: history, integrity, storage, retention, and the tools a project needs.
        // Reads first; every write below is named as one and asks the page for a confirmation.
        ("GET", "/api/history") => history(ctx, req),
        ("GET", "/api/project/file") => project_file(ctx, req),
        ("GET", "/api/project/why") => project_why(ctx, req),
        ("GET", "/api/project/blame") => project_blame(ctx, req),
        ("GET", "/api/project/diff") => project_diff(ctx, req),
        ("GET", "/api/project/last-good") => project_last_good(ctx, req),
        ("POST", "/api/project/preview") => restore_preview(ctx, req),
        ("GET", "/api/check") => check(ctx, req),
        ("GET", "/api/size") => size_now(ctx),
        ("GET", "/api/gc") => gc_now(ctx, req),
        ("GET", "/api/audit") => audit_archive(ctx),
        ("GET", "/api/quarantine") => quarantine_list(ctx),
        ("GET", "/api/retention") => retention_get(ctx, req),
        ("GET", "/api/recent") => recent_now(ctx),
        ("GET", "/api/suggest") => core_read(ctx, vec!["suggest".to_string()]),
        ("GET", "/api/config/all") => config_all(ctx),
        ("POST", "/api/project/note") => project_note(ctx, req),
        ("POST", "/api/project/mark") => project_mark(ctx, req),
        ("POST", "/api/project/relink") => project_relink(ctx, req),
        ("POST", "/api/project/remove") => project_remove(ctx, req),
        ("POST", "/api/project/apply-filters") => project_apply_filters(ctx, req),
        ("POST", "/api/retention/set") => retention_set(ctx, req),
        ("POST", "/api/retention/apply") => retention_apply(ctx, req),
        ("POST", "/api/recover") => recover_now(ctx, req),
        ("POST", "/api/drill") => drill_start(ctx, req),
        ("GET", "/api/notifications") => notifications(ctx, req),
        ("POST", "/api/project/repair") => repair_start(ctx, req),
        ("GET", "/api/menu") => menu_json(ctx, req),
        ("POST", "/api/menu/run") => menu_run(ctx, req),
        _ => err(404, "no such route"),
    }
}

// ------------------------------------------------------------------------------------------
// State

fn bootstrap(ctx: &Arc<Ctx>) -> Response {
    let core = ctx.core();
    let root = ctx.archive_root();
    let loc = pl::location_file(ctx.home.as_deref());
    let mut archive = json!({
        "configured": root.is_some(),
        "root": root.as_ref().map(|r| r.to_string_lossy().to_string()),
        "locationFile": loc.to_string_lossy(),
        "exists": root.as_ref().map(|r| r.is_dir()).unwrap_or(false),
        "isArchive": root.as_ref().map(|r| pl::is_archive(r)).unwrap_or(false),
        "freeBytes": Value::Null,
        "totalBytes": Value::Null,
    });
    if let Some(r) = &root {
        archive["freeBytes"] = json!(pl::free_bytes(r));
        archive["totalBytes"] = json!(pl::total_bytes(r));
    }

    let mut projects: Vec<Value> = Vec::new();
    if archive["isArchive"].as_bool().unwrap_or(false) {
        match core.json(&["status".into(), "--json".into()]) {
            Ok(Value::Array(rows)) => {
                let root_dev = root.clone();
                for mut row in rows {
                    if let (Some(rr), Some(pr)) = (&root_dev, row.get("projectRoot").and_then(|v| v.as_str())) {
                        let same = pl::same_device(rr, Path::new(pr));
                        row["sameVolume"] = json!(same);
                    }
                    row["stateLabel"] = json!(state_label(row.get("state").and_then(|v| v.as_str()).unwrap_or("")));
                    projects.push(row);
                }
            }
            _ => {}
        }
    }

    let guard = if ctx.native { "app" } else { "app-server" };

    let mut watch = json!({
        "runningByApp": daemon_child_alive(ctx),
        "childPid": ctx.daemon.lock().ok().and_then(|g| g.as_ref().map(|c| c.id())),
        "selfStoppedMs": ctx.self_stopped_ms.lock().ok().and_then(|v| *v),
        "heartbeat": Value::Null,
        "trigger": Value::Null,
        "mode": "unknown",
    });
    if archive["isArchive"].as_bool().unwrap_or(false) {
        if let Ok(v) = core.json(&["heartbeat-check".into(), "--json".into()]) {
            watch["mode"] = v.get("mode").cloned().unwrap_or(json!("unknown"));
            watch["heartbeat"] = v;
        } else {
            // `heartbeat-check` exits non-zero when the heartbeat is stale; the JSON is still there.
            let (_c, out, _e) = core.run(&["heartbeat-check".into(), "--json".into()]);
            if let Some(v) = pl::last_json(&out) {
                watch["mode"] = v.get("mode").cloned().unwrap_or(json!("unknown"));
                watch["heartbeat"] = v;
            }
        }
        if let Some(r) = &root {
            if let Ok(text) = std::fs::read_to_string(r.join("watch_state.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    watch["trigger"] = v;
                }
            }
        }
    }

    // The same answer the /api/watch route gives, so the window cannot hold two opinions about
    // whether it is protecting anything (round 296: it held two).
    {
        let by_app = watch["runningByApp"].as_bool().unwrap_or(false);
        let self_stopped = watch["selfStoppedMs"].as_i64();
        let hb = watch.get("heartbeat").filter(|v| !v.is_null()).cloned();
        let prot = protection(hb.as_ref(), by_app, self_stopped);
        watch["storage"] = prot.get("storage").cloned().unwrap_or(Value::Null);
        watch["protection"] = prot;
    }

    let config = read_config(root.as_deref());
    let version = core.version();
    let mut program = brand(ctx);
    program["coreVersion"] = json!(version);
    ok(json!({
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "coreVersion": version,
            "native": ctx.native,
            "platform": std::env::consts::OS,
            "guard": guard,
            "now": pl::now_ms(),
            "nowLocal": pl::fmt_local(pl::now_ms()),
            // Who made this and what it is for. The values are not written here: the core owns
            // them (`src/brand.rs`) and answers with them, so the page, the menus and the licence
            // cannot hold three opinions about one name.
            "product": program["product"].clone(),
            "author": program["author"].clone(),
            "authorEmail": program["authorEmail"].clone(),
            "by": program["by"].clone(),
            "whatItIs": program["whatItIs"].clone(),
            "whatItIsRu": program["whatItIsRu"].clone(),
            "licence": program["licence"].clone(),
            "copyright": program["copyright"].clone(),
        },
        "program": program,
        "archive": archive,
        "projects": projects,
        "watch": watch,
        "config": config,
        "jobs": ctx.jobs.list().iter().map(|j| j.to_json()).collect::<Vec<_>>(),
        "daemonLog": ctx.daemon_log.to_string_lossy(),
        "build": build_info(ctx),
    }))
}

pub fn state_label(state: &str) -> &'static str {
    match state {
        "active" => "protected",
        "paused" => "paused",
        "initializing" => "initializing",
        "path_missing" => "path missing",
        "error" => "error",
        _ => "unknown",
    }
}

/// Who made this and what it is for — asked of the core, not written here a second time.
///
/// The values live in the core's own `src/brand.rs`, and the core prints them: this asks
/// `projectlife version --json` and remembers the answer, because the answer cannot change while a
/// given core binary is on disk and the page asks for the bootstrap every three seconds. The cache
/// is keyed by the core's path, so a different binary is a different question.
///
/// If the core cannot answer, the block says *that* instead of inventing a name: an About screen that
/// guesses is worse than one that admits it could not read the answer.
pub fn brand(ctx: &Arc<Ctx>) -> Value {
    static BRAND: OnceLock<Mutex<Option<(PathBuf, Value)>>> = OnceLock::new();
    let cell = BRAND.get_or_init(|| Mutex::new(None));
    let mut g = match cell.lock() {
        Ok(g) => g,
        // A poisoned lock is not worth failing a bootstrap over: read the answer again.
        Err(_) => return read_brand(ctx),
    };
    if let Some((path, value)) = g.as_ref() {
        if *path == ctx.bin {
            return value.clone();
        }
    }
    let v = read_brand(ctx);
    *g = Some((ctx.bin.clone(), v.clone()));
    v
}

fn read_brand(ctx: &Arc<Ctx>) -> Value {
    let args = vec!["version".to_string(), "--json".to_string()];
    match ctx.core().json(&args) {
        Ok(v) => v,
        Err(e) => json!({
            "unavailable": true,
            "why": format!("the core did not answer `projectlife version --json`: {e}"),
        }),
    }
}

pub fn read_config(root: Option<&Path>) -> Value {
    let mut cfg = json!({
        "intervalSeconds": 5,
        "notifications": true,
        "watchTriggers": true,
    });
    if let Some(r) = root {
        if let Ok(text) = std::fs::read_to_string(r.join("config.json")) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) {
                for (k, v) in m {
                    cfg[&k] = v;
                }
            }
        }
    }
    cfg
}

fn daemon_child_alive(ctx: &Arc<Ctx>) -> bool {
    let mut g = match ctx.daemon.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if let Some(child) = g.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => {
                *g = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    } else {
        false
    }
}

// ------------------------------------------------------------------------------------------
// Store

fn archive_use(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let path = match req.str_field("path") {
        Some(p) => p,
        None => return err(400, "path is required"),
    };
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return err(400, &format!("not a folder: {path}"));
    }
    if !pl::is_archive(&root) {
        return err(
            400,
            "this folder is not a Project Life archive yet — choose “Create the archive here” instead",
        );
    }
    ctx.set_archive(Some(root.clone()));
    ok(json!({"archiveRoot": root.to_string_lossy(), "isArchive": true}))
}

/// The window asks the shell for a native folder picker; on Linux (headless tests) the caller has
/// already supplied the path, so this route simply reports that there is no native picker.
fn archive_pick(ctx: &Arc<Ctx>, _req: &Request) -> Response {
    if ctx.native {
        ok(json!({"native": true, "hint": "the shell answers pl://pick-folder"}))
    } else {
        err(409, "no native folder picker in this build; supply the path directly")
    }
}

fn archive_init(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let path = match req.str_field("path") {
        Some(p) => p,
        None => return err(400, "path is required"),
    };
    let root = PathBuf::from(&path);
    if root.exists() && !root.is_dir() {
        return err(400, &format!("{path} is a file, not a folder"));
    }
    if pl::is_archive(&root) {
        ctx.set_archive(Some(root.clone()));
        return ok(json!({"archiveRoot": path, "created": false, "already": true}));
    }
    let core = Core { bin: ctx.bin.clone(), archive: None, home: ctx.home.clone() };
    let (code, out, e) = core.run(&["init-archive".into(), path.clone()]);
    if code != 0 {
        return err(500, &format!("could not create the archive: {}{}", pl::first_lines(&out, 2), e));
    }
    ctx.set_archive(Some(root));
    ok(json!({"archiveRoot": path, "created": true, "output": out.trim()}))
}

// ------------------------------------------------------------------------------------------
// Detection and the add wizard

fn detect(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let path = match req.str_field("path") {
        Some(p) => p,
        None => return err(400, "path is required"),
    };
    let core = ctx.core();
    match core.json(&["detect".into(), path.clone(), "--json".into()]) {
        Ok(v) => ok(v),
        Err(e) => err(500, &e),
    }
}

fn presets(ctx: &Arc<Ctx>) -> Response {
    let core = ctx.core();
    match core.json(&["presets".into(), "--json".into()]) {
        Ok(v) => ok(v),
        Err(e) => err(500, &e),
    }
}

fn add_args(ctx: &Arc<Ctx>, req: &Request, dry: bool) -> Result<Vec<String>, Response> {
    if ctx.archive_root().is_none() {
        return Err(err(409, "no archive yet — choose where to keep the archive first"));
    }
    let path = req.str_field("path").ok_or_else(|| err(400, "path is required"))?;
    let preset = req.str_field("preset").unwrap_or_else(|| "custom".into());
    let mut args: Vec<String> = vec!["add".into(), path, "--preset".into(), preset];
    if let Some(name) = req.str_field("name") {
        if !name.trim().is_empty() {
            args.push("--name".into());
            args.push(name);
        }
    }
    for a in req.str_list("editAdd") {
        args.push("--edit-add".into());
        args.push(a);
    }
    for r in req.str_list("editRemove") {
        args.push("--edit-remove".into());
        args.push(r);
    }
    if req.json_body().get("includeLargeFiles").and_then(|v| v.as_bool()).unwrap_or(false) {
        args.push("--include-large-files".into());
    }
    if dry {
        args.push("--dry-run".into());
        args.push("--json".into());
    } else {
        args.push("--yes".into());
    }
    Ok(args)
}

fn add_preview(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let args = match add_args(ctx, req, true) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let core = ctx.core();
    let (code, out, e) = core.run(&args);
    if code != 0 {
        let msg = pl::last_json(&out)
            .and_then(|v| v.get("error").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| format!("{}", pl::first_lines(&e, 3)));
        return err(400, &msg);
    }
    match pl::last_json(&out) {
        Some(v) => ok(v),
        None => err(500, "the core did not return an estimate"),
    }
}

fn add_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let args = match add_args(ctx, req, false) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let path = req.str_field("path").unwrap_or_default();
    if let Some(id) = ctx.jobs.running_of_kind("add") {
        return err(409, &format!("an add is already running (job {id})"));
    }
    let ctx2 = Arc::clone(ctx);
    let id = ctx.jobs.start("add", &format!("Add folder {path}"), move |h| add_job(&ctx2, &args, h));
    ok(json!({"job": id}))
}

fn add_job(ctx: &Arc<Ctx>, args: &[String], h: &JobHandle) -> Result<Value, String> {
    let core = ctx.core();
    let (code, out) = jobs::run_streaming(&core, args, h)?;
    if code != 0 {
        return Err(format!("projectlife {} exited {code}", args.join(" ")));
    }
    // What was added is read back from the core, not from the text we just printed.
    let rows = core.json(&["status".into(), "--json".into()]).unwrap_or(json!([]));
    let path = args.get(1).cloned().unwrap_or_default();
    let mut found = Value::Null;
    if let Value::Array(a) = rows {
        for r in a {
            if r.get("projectRoot").and_then(|v| v.as_str()) == Some(path.as_str()) {
                found = r;
                break;
            }
        }
    }
    let _ = out;
    Ok(json!({"project": found, "exit": code}))
}

// ------------------------------------------------------------------------------------------
// Project: moments, tree, restore

fn project_detail(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.param("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let core = ctx.core();
    let log = match core.json(&["log".into(), name.clone(), "--json".into()]) {
        Ok(v) => v,
        Err(e) => return err(500, &e),
    };
    let events = log.as_array().cloned().unwrap_or_default();
    let mut moments: Vec<Value> = Vec::new();
    let mut by_ts: std::collections::BTreeMap<i64, (usize, usize, usize, usize)> = std::collections::BTreeMap::new();
    let mut skipped: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut gaps: Vec<Value> = Vec::new();
    for e in &events {
        let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = e.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        match t {
            "put" => {
                let ent = by_ts.entry(ts).or_default();
                ent.0 += 1;
                if let Some(p) = e.get("path").and_then(|v| v.as_str()) {
                    files.insert(p.to_string());
                }
            }
            "delete" => by_ts.entry(ts).or_default().1 += 1,
            "move" => by_ts.entry(ts).or_default().2 += 1,
            "skip" => {
                by_ts.entry(ts).or_default().3 += 1;
                let r = e.get("reason").and_then(|v| v.as_str()).unwrap_or("other").to_string();
                *skipped.entry(r).or_insert(0) += 1;
            }
            "gap" => gaps.push(json!({
                "from": e.get("from").and_then(|v| pl::parse_iso_utc(v.as_str().unwrap_or(""))).map(pl::fmt_local),
                "to": e.get("to").and_then(|v| pl::parse_iso_utc(v.as_str().unwrap_or(""))).map(pl::fmt_local),
                "reason": e.get("reason").and_then(|v| v.as_str()),
            })),
            _ => {}
        }
    }
    for (ts, (puts, dels, moves, skips)) in &by_ts {
        if *puts + *dels + *moves + *skips == 0 {
            continue;
        }
        moments.push(json!({
            "at": ts,
            "atIso": ms_to_iso(*ts),
            "atLocal": pl::fmt_local(*ts),
            "puts": puts, "deletes": dels, "moves": moves, "skips": skips,
            "changes": puts + dels + moves,
        }));
    }
    moments.reverse();
    ok(json!({
        "name": name,
        "events": events.len(),
        "moments": moments,
        "skippedByReason": skipped,
        "gaps": gaps,
        "fileCount": files.len(),
    }))
}

fn project_tree(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.param("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let at = match req.param("at") {
        Some(a) => a,
        None => return err(400, "at is required"),
    };
    let core = ctx.core();
    let args = vec!["tree".to_string(), name, "--at".to_string(), at.clone(), "--json".to_string()];
    match core.json(&args) {
        Ok(v) => ok(json!({"at": at, "files": v})),
        Err(e) => err(400, &e),
    }
}

fn restore_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = req.str_field("name").ok_or_else(|| err(400, "name is required"));
    let name = match name {
        Ok(n) => n,
        Err(r) => return r,
    };
    let at = match req.str_field("at") {
        Some(a) => a,
        None => return err(400, "at is required"),
    };
    let to = match req.str_field("to") {
        Some(t) => t,
        None => return err(400, "to is required"),
    };
    let paths = req.str_list("paths");
    if paths.is_empty() {
        return err(400, "select at least one file to restore");
    }
    let ctx2 = Arc::clone(ctx);
    let label = format!("Restore {name} at {at}");
    let id = ctx.jobs.start("restore", &label, move |h| restore_job(&ctx2, &name, &at, &to, &paths, h));
    ok(json!({"job": id}))
}

fn restore_job(
    ctx: &Arc<Ctx>,
    name: &str,
    at: &str,
    to: &str,
    paths: &[String],
    h: &JobHandle,
) -> Result<Value, String> {
    let core = ctx.core();
    // The expected content comes from the archive's own tree, read before anything is written.
    let tree = core.json(&["tree".to_string(), name.to_string(), "--at".to_string(), at.to_string(), "--json".to_string()])?;
    let mut expected: std::collections::BTreeMap<String, (String, String, u64)> = std::collections::BTreeMap::new();
    if let Value::Array(rows) = &tree {
        for r in rows {
            let p = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let hash = r.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("file").to_string();
            let size = r.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            expected.insert(p, (hash, kind, size));
        }
    }
    let mut args: Vec<String> = vec!["restore".into(), name.to_string(), "--at".into(), at.to_string(), "--to".into(), to.to_string()];
    for p in paths {
        args.push("--path".into());
        args.push(p.clone());
    }
    args.push("--yes".into());
    let (code, out) = jobs::run_streaming(&core, &args, h)?;
    if code != 0 {
        return Err(format!("projectlife restore exited {code}"));
    }
    // Independent check: hash what came out and compare it with what the archive recorded.
    let mut checked = 0usize;
    let mut mismatched: Vec<Value> = Vec::new();
    let mut skipped_links = 0usize;
    for p in paths {
        let exp = match expected.get(p) {
            Some(e) => e,
            None => {
                mismatched.push(json!({"path": p, "why": "not in the tree at that moment"}));
                continue;
            }
        };
        let target = Path::new(to).join(p);
        if exp.1 == "symlink" {
            skipped_links += 1;
            continue;
        }
        match pl::sha256_file(&target) {
            Ok(actual) => {
                if actual == exp.0 {
                    checked += 1;
                } else {
                    mismatched.push(json!({"path": p, "why": "content differs", "expected": exp.0, "actual": actual}));
                }
            }
            Err(e) => mismatched.push(json!({"path": p, "why": format!("cannot read the restored file: {e}")})),
        }
    }
    h.line(&format!(
        "verified independently: {checked} file(s) byte-for-byte, {} mismatch(es), {skipped_links} symlink(s) not compared",
        mismatched.len()
    ));
    Ok(json!({
        "target": to,
        "requested": paths.len(),
        "verified": checked,
        "mismatched": mismatched,
        "symlinksNotCompared": skipped_links,
        "output": out,
        "exit": code,
    }))
}

// ------------------------------------------------------------------------------------------
// Export / import

fn export_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let out = match req.str_field("out") {
        Some(o) => o,
        None => return err(400, "out is required"),
    };
    let ctx2 = Arc::clone(ctx);
    let label = format!("Export {name}");
    let id = ctx.jobs.start("export", &label, move |h| export_job(&ctx2, &name, &out, h));
    ok(json!({"job": id}))
}

fn export_job(ctx: &Arc<Ctx>, name: &str, out: &str, h: &JobHandle) -> Result<Value, String> {
    let core = ctx.core();
    let args = vec!["export".to_string(), name.to_string(), "--out".to_string(), out.to_string()];
    let (code, text) = jobs::run_streaming(&core, &args, h)?;
    if code != 0 {
        return Err(format!("projectlife export exited {code} (a failed export is never pruned from)"));
    }
    // Verify the export the way its own manifest says to, but from this side.
    let manifest = Path::new(out).join("MANIFEST.sha256");
    let text_m = std::fs::read_to_string(&manifest).map_err(|e| format!("no manifest at {}: {e}", manifest.display()))?;
    let mut lines = 0usize;
    let mut okn = 0usize;
    let mut bad: Vec<Value> = Vec::new();
    for line in text_m.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (hash, path) = match line.split_once("  ") {
            Some((a, b)) => (a, b),
            None => match line.split_once(' ') {
                Some((a, b)) => (a, b.trim_start()),
                None => continue,
            },
        };
        lines += 1;
        let f = Path::new(out).join(path);
        match pl::sha256_file(&f) {
            Ok(actual) if actual == hash => okn += 1,
            Ok(actual) => bad.push(json!({"path": path, "expected": hash, "actual": actual})),
            Err(e) => bad.push(json!({"path": path, "why": e})),
        }
    }
    h.line(&format!("manifest re-checked here: {okn}/{lines} file(s) match their recorded hash"));
    Ok(json!({
        "out": out,
        "manifestLines": lines,
        "manifestOk": okn,
        "manifestBad": bad,
        "output": text,
        "exit": code,
    }))
}

fn import_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let dir = match req.str_field("dir") {
        Some(d) => d,
        None => return err(400, "dir is required"),
    };
    let new_name = req.str_field("newName").filter(|s| !s.trim().is_empty());
    let ctx2 = Arc::clone(ctx);
    let label = format!("Import {dir}");
    let id = ctx.jobs.start("import", &label, move |h| import_job(&ctx2, &dir, new_name, h));
    ok(json!({"job": id}))
}

fn import_job(ctx: &Arc<Ctx>, dir: &str, new_name: Option<String>, h: &JobHandle) -> Result<Value, String> {
    let core = ctx.core();
    let mut args = vec!["import".to_string(), dir.to_string()];
    // `--new` is the only form that is safe for a project that already has a journal.
    args.push("--new".into());
    args.push(new_name.clone().unwrap_or_default());
    let (code, text) = jobs::run_streaming(&core, &args, h)?;
    if code != 0 {
        return Err(format!("projectlife import exited {code}"));
    }
    // Exported events on disk, against the events the archive now holds.
    let mut exported_events = 0usize;
    if let Ok(rd) = std::fs::read_dir(Path::new(dir).join("events")) {
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_to_string(e.path()) {
                exported_events += t.lines().count();
            }
        }
    }
    let rows = core.json(&["status".into(), "--json".into()]).unwrap_or(json!([]));
    let mut imported: Value = Value::Null;
    if let Value::Array(a) = &rows {
        for r in a {
            if Some(r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string())
                == new_name.clone().or_else(|| r.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
            {
                imported = r.clone();
            }
        }
        if imported.is_null() && !a.is_empty() {
            imported = a[a.len() - 1].clone();
        }
    }
    h.line(&format!("exported event lines on disk: {exported_events}"));
    Ok(json!({
        "project": imported,
        "exportedEvents": exported_events,
        "output": text,
        "exit": code,
    }))
}

// ------------------------------------------------------------------------------------------
// Watching

fn watch_state(ctx: &Arc<Ctx>) -> Response {
    let core = ctx.core();
    let (_c, out, _e) = core.run(&["heartbeat-check".into(), "--json".into()]);
    let hb = pl::last_json(&out);
    let trigger = ctx
        .archive_root()
        .and_then(|r| std::fs::read_to_string(r.join("watch_state.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let by_app = daemon_child_alive(ctx);
    let self_stopped = ctx.self_stopped_ms.lock().ok().and_then(|v| *v);
    let protection = protection(hb.as_ref(), by_app, self_stopped);
    // Round 300 (FR-CFG-3): what a *running* process did about its configuration. The daemon writes
    // this file precisely because its own counter is not reachable from here — so the window shows
    // what the daemon recorded, or nothing at all if no daemon has ever re-read.
    let config_state = ctx
        .archive_root()
        .and_then(|r| std::fs::read_to_string(r.join("config_state.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    ok(json!({
        "runningByApp": by_app,
        "childPid": ctx.daemon.lock().ok().and_then(|g| g.as_ref().map(|c| c.id())),
        "selfStoppedMs": self_stopped,
        "heartbeat": hb,
        "storage": protection.get("storage").cloned().unwrap_or(Value::Null),
        "protection": protection,
        "trigger": trigger,
        "configState": config_state,
        "intervalSeconds": read_config(ctx.archive_root().as_deref()).get("intervalSeconds").cloned().unwrap_or(json!(5)),
        "build": build_info(ctx),
    }))
}

/// Which build is this? — asked of the files themselves.
///
/// The owner's Mac test of 2026-10-06 was reported against a bundle that had been replaced hours
/// earlier, and nothing on screen said which one he had opened: a whole round went into working out
/// that the three failures he described were the three that had already been fixed. A version in a
/// filename cannot settle that, so this hashes the three things the running app is made of — the
/// interface server (this process), the core binary it calls, and the page it serves — and prints
/// them. `shasum -a 256 "…/Contents/Resources/projectlife"` on the Mac must equal `core.sha256`.
fn build_info(ctx: &Arc<Ctx>) -> Value {
    fn one(path: Option<PathBuf>) -> Value {
        match path {
            None => Value::Null,
            Some(p) => match pl::file_hash_cached(&p) {
                Ok((bytes, sha)) => json!({
                    "path": p.to_string_lossy(),
                    "bytes": bytes,
                    "sha256": sha,
                    "short": sha.chars().take(8).collect::<String>(),
                }),
                Err(e) => json!({"path": p.to_string_lossy(), "error": e}),
            },
        }
    }
    let server = one(std::env::current_exe().ok());
    let core = one(Some(ctx.bin.clone()));
    let page_sha = pl::sha256_bytes(crate::assets::APP_JS.as_bytes());
    let page = json!({
        "bytes": crate::assets::APP_JS.len(),
        "sha256": page_sha,
        "short": page_sha.chars().take(8).collect::<String>(),
    });
    let short = format!(
        "app {} · ui {} · core {}",
        env!("CARGO_PKG_VERSION"),
        server["short"].as_str().unwrap_or("?"),
        core["short"].as_str().unwrap_or("?")
    );
    json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "short": short,
        "server": server,
        "core": core,
        "page": page,
        "check": "compare with: shasum -a 256 \"Project Life.app/Contents/Resources/projectlife\" \"Project Life.app/Contents/Resources/projectlife-ui\"",
    })
}

/// One rule, in one place: is the promise being kept *right now*?
///
/// The window and the menu-bar item both ask this question, and on 2026-10-06 they both answered
/// "Protected" while the daemon was refusing every write — the daemon was alive, so the window
/// assumed the archive was being written to. Liveness is not protection. Neither is a fresh
/// heartbeat on a disk that has no room: an observation that cannot store anything has kept no
/// promise, and saying so is the whole job of this function.
///
/// `state` is the machine-readable answer; `reason` is the core's own sentence with its numbers.
pub fn protection(hb: Option<&Value>, by_app: bool, self_stopped: Option<i64>) -> Value {
    let hb = match hb {
        Some(v) => v,
        None => {
            return json!({
                "state": "unknown", "protected": false,
                "label": "Unknown",
                "reason": "the core program did not answer heartbeat-check, so nothing can be said about observation",
                "storage": Value::Null, "mode": "unknown", "fresh": false, "ageMs": Value::Null,
            })
        }
    };
    let mode = hb.get("mode").and_then(|m| m.as_str()).unwrap_or("unknown");
    let fresh = hb.get("fresh").and_then(|f| f.as_bool()).unwrap_or(false);
    let age = hb.get("ageMs").cloned().unwrap_or(Value::Null);
    let max_age = hb.get("maxAgeMs").cloned().unwrap_or(json!(60_000));
    let storage = hb.get("storage").cloned().unwrap_or(Value::Null);
    let storage_stop = storage.get("stop").and_then(|v| v.as_bool()).unwrap_or(false);
    let storage_warn = storage.get("warn").and_then(|v| v.as_bool()).unwrap_or(false);
    let storage_reason = storage.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let age_text = match age.as_i64() {
        Some(ms) => format!("last observation {} s ago", ms / 1000),
        None => "nothing has ever been observed".to_string(),
    };
    let base = |state: &str, protected: bool, label: &str, reason: String| -> Value {
        json!({
            "state": state, "protected": protected, "label": label, "reason": reason,
            "storage": storage, "mode": mode, "fresh": fresh, "ageMs": age,
            "maxAgeMs": max_age, "intervalMs": hb.get("intervalMs").cloned().unwrap_or(json!(5000)),
            "stoppedByAppMs": self_stopped,
        })
    };

    // 1. Nothing is being written, whatever else is true: the disk decides this, not the process.
    if storage_stop {
        return base(
            "paused_full",
            false,
            "Paused — the archive is full",
            format!(
                "{storage_reason}. Observation is still running and will resume writing by itself when space is freed; nothing is deleted."
            ),
        );
    }
    // 2. The app stopped observation itself: a heartbeat from moments ago is not protection.
    if self_stopped.is_some() && !by_app {
        return base(
            "stopped",
            false,
            "Observation stopped",
            format!("observation was stopped from this window; {age_text}"),
        );
    }
    // 3. Something is observing and can write. That, and only that, is protection.
    let running = by_app || mode.starts_with("daemon") || mode == "external timer";
    if running && fresh {
        let who = if by_app {
            "this app is observing"
        } else if mode == "external timer" {
            "an external timer is observing"
        } else {
            "a daemon started outside this window is observing"
        };
        let reason = if storage_warn {
            format!("{who}; {age_text}. {storage_reason}")
        } else {
            format!("{who}; {age_text}")
        };
        return base(if storage_warn { "protected_low_space" } else { "protected" }, true, "Protected", reason);
    }
    // 4. A process that claims to be running while nothing has been observed is its own failure.
    if running && !fresh {
        return base(
            "stale",
            false,
            "Not protecting — observation has stopped reporting",
            format!("{mode} is running but {age_text}, and the limit is {} s", max_age.as_i64().unwrap_or(60_000) / 1000),
        );
    }
    base(
        "stopped",
        false,
        "Not protecting",
        format!("observation is stopped; {age_text}"),
    )
}

fn watch_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let root = match ctx.archive_root() {
        Some(r) => r,
        None => return err(409, "no archive yet"),
    };
    if daemon_child_alive(ctx) {
        return ok(json!({"already": true, "runningByApp": true}));
    }
    if let Some(secs) = req.u64_field("intervalSeconds") {
        let core = ctx.core();
        let (code, out, e) = core.run(&["config".into(), "set".into(), "intervalSeconds".into(), secs.to_string()]);
        if code != 0 {
            return err(500, &format!("cannot write the interval: {out}{e}"));
        }
    }
    let core = ctx.core();
    let mut cmd = Command::new(&core.bin);
    cmd.arg("--archive").arg(&root);
    if let Some(h) = &ctx.home {
        cmd.env("PROJECTLIFE_HOME", h);
    }
    cmd.arg("daemon").arg("run");
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            // Keep the daemon's own words: they are what explains a refusal to a person.
            if let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) {
                let path = ctx.daemon_log.clone();
                std::thread::spawn(move || {
                    use std::io::{BufRead, BufReader, Write};
                    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
                    let mut pump = |r: Box<dyn std::io::Read + Send>| {
                        let rd = BufReader::new(r);
                        for line in rd.lines().map_while(Result::ok) {
                            if let Some(f) = f.as_mut() {
                                let _ = writeln!(f, "{line}");
                            }
                        }
                    };
                    pump(Box::new(out));
                    pump(Box::new(err));
                });
            }
            let mut g = ctx.daemon.lock().unwrap();
            *g = Some(child);
            drop(g);
            if let Ok(mut v) = ctx.self_stopped_ms.lock() {
                *v = None;
            }
            ok(json!({"started": true, "pid": pid, "archive": root.to_string_lossy()}))
        }
        Err(e) => err(500, &format!("cannot start observation: {e}")),
    }
}

/// Stop the daemon this app started. Called when the process is leaving.
pub fn stop_daemon_for_exit(ctx: &Arc<Ctx>) -> bool {
    stop_daemon(ctx)
}

fn stop_daemon(ctx: &Arc<Ctx>) -> bool {
    let mut g = match ctx.daemon.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if let Some(mut child) = g.take() {
        // Ask first in the way every platform has: the core's own stop request, the file the daemon
        // reads on each cycle (`projectlife daemon stop` writes the same one). A signal is the
        // unix half of the same idea and is sent too; TerminateProcess — `child.kill()` — is what is
        // left for a daemon that ignored both, and it is never the first move, because it can land
        // in the middle of a write.
        let asked = match ctx.archive_root() {
            Some(root) => {
                let mut cmd = Command::new(&ctx.core().bin);
                cmd.arg("--archive").arg(&root);
                if let Some(h) = &ctx.home {
                    cmd.env("PROJECTLIFE_HOME", h);
                }
                cmd.arg("daemon").arg("stop");
                cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                crate::sys::detach(&mut cmd);
                cmd.status().map(|s| s.success()).unwrap_or(false)
            }
            None => false,
        };
        let _ = crate::sys::terminate(child.id());
        let _ = asked;
        let deadline = pl::now_ms() + 15_000;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if pl::now_ms() < deadline => std::thread::sleep(std::time::Duration::from_millis(120)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return true;
                }
                Err(_) => return true,
            }
        }
    }
    false
}

fn watch_stop(ctx: &Arc<Ctx>) -> Response {
    if stop_daemon(ctx) {
        if let Ok(mut v) = ctx.self_stopped_ms.lock() {
            *v = Some(pl::now_ms());
        }
        return ok(json!({"stopped": true, "by": "app"}));
    }
    // Nothing of ours is running. If something else is observing this archive, say so instead of
    // killing a process this app did not start.
    let core = ctx.core();
    let (_c, out, _e) = core.run(&["heartbeat-check".into(), "--json".into()]);
    let hb = pl::last_json(&out);
    let mode = hb.as_ref().and_then(|v| v.get("mode").and_then(|m| m.as_str())).unwrap_or("unknown").to_string();
    let holder = hb
        .as_ref()
        .and_then(|v| v.get("daemonHolder").and_then(|m| m.as_str()))
        .unwrap_or("")
        .to_string();
    if mode.starts_with("daemon") {
        return err(
            409,
            &format!("observation is running, but it was started outside this app ({holder}); this app will not kill another program's process"),
        );
    }
    ok(json!({"stopped": false, "wasRunning": false}))
}

// ------------------------------------------------------------------------------------------
// Diagnostics

/// `pl config set <key> <value>`: the same settings file the terminal uses.
fn config_set(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let key = match req.str_field("key") {
        Some(k) => k,
        None => return err(400, "key is required"),
    };
    let body = req.json_body();
    let raw = match body.get("value") {
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => return err(400, "value is required"),
    };
    let core = ctx.core();
    let (code, out, e) = core.run(&["config".into(), "set".into(), key.clone(), raw.clone()]);
    if code != 0 {
        return err(500, &format!("cannot set {key}: {out}{e}"));
    }
    ok(json!({"key": key, "value": raw, "output": out.trim()}))
}

/// One observation pass over every project, exactly what a scheduler would run.
fn pass_start(ctx: &Arc<Ctx>) -> Response {
    if ctx.archive_root().is_none() {
        return err(409, "no archive yet");
    }
    let ctx2 = Arc::clone(ctx);
    let ctx_for_job = Arc::clone(ctx);
    let id = ctx2.jobs.start("pass", "Observe once now", move |h| {
        let core = ctx_for_job.core();
        let args = vec!["scan-once".to_string(), "--all".to_string()];
        let (code, _out) = jobs::run_streaming(&core, &args, h)?;
        if code != 0 {
            return Err(format!("projectlife scan-once exited {code}"));
        }
        let rows = core.json(&["status".into(), "--json".into()]).unwrap_or(json!([]));
        Ok(json!({"projects": rows}))
    });
    ok(json!({"job": id}))
}

fn pause_project(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let pause = req.json_body().get("pause").and_then(|v| v.as_bool()).unwrap_or(true);
    let core = ctx.core();
    let verb = if pause { "pause" } else { "resume" };
    let (code, out, e) = core.run(&[verb.to_string(), name.clone()]);
    if code != 0 {
        return err(500, &format!("cannot {verb} {name}: {out}{e}"));
    }
    ok(json!({"name": name, "state": if pause { "paused" } else { "active" }, "output": out.trim()}))
}

fn doctor(ctx: &Arc<Ctx>) -> Response {
    let core = ctx.core();
    let (_c, out, _e) = core.run(&["doctor".into(), "--json".into()]);
    let checks = pl::last_json(&out).unwrap_or(json!([]));
    let (_c2, hout, _e2) = core.run(&["healthcheck".into(), "--json".into()]);
    let health = pl::last_json(&hout).unwrap_or(Value::Null);
    ok(json!({"checks": checks, "health": health}))
}

/// The app's own log file: the daemon's output and every core command the UI ran.
fn raw_log(ctx: &Arc<Ctx>) -> Response {
    let text = std::fs::read_to_string(&ctx.daemon_log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(200).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    ok(json!({"log": ctx.daemon_log.to_string_lossy(), "lines": tail}))
}

fn ms_to_iso(ms: i64) -> String {
    // The journal's own format, so the value can be handed straight back to `--at`.
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


// ==========================================================================================
// Round 297 — the rest of the core's surface.
//
// The audit behind this section is docs/UI_COVERAGE.md: every command the core has, where the
// window shows it, and what is deliberately left to the terminal. The rule for every route here
// is the same as everywhere else in this file: the app computes nothing itself. It asks the core
// for `--json` and gives the window exactly what came back, so the screen cannot disagree with
// the program about a number.
// ==========================================================================================

fn core_read(ctx: &Arc<Ctx>, mut args: Vec<String>) -> Response {
    args.push("--json".into());
    match ctx.core().json(&args) {
        Ok(v) => ok(v),
        Err(e) => err(400, &e),
    }
}

/// A write. It reports "done" only when the core exited 0, and when it did not, the core's own
/// sentence is what the window shows — not a rephrasing of it here.
fn core_write(ctx: &Arc<Ctx>, args: &[String]) -> Response {
    let core = ctx.core();
    let (code, out, e) = core.run(args);
    if code != 0 {
        let msg = pl::last_json(&out)
            .and_then(|v| v.get("error").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| pl::first_lines(&e, 3));
        return err(if code == 3 { 409 } else { 400 }, &msg);
    }
    match pl::last_json(&out) {
        Some(v) => ok(json!({"ok": true, "result": v, "exit": code})),
        None => ok(json!({"ok": true, "output": out.trim(), "exit": code})),
    }
}

fn req_name(req: &Request) -> Result<String, Response> {
    match req.param("name") {
        Some(n) if !n.is_empty() => Ok(n),
        _ => Err(err(400, "name is required")),
    }
}

/// The page has to say, in the request, that the person agreed. The core asks for the same
/// agreement on its own side (`--yes`); a route that writes without one of the two would be
/// guessing on someone else's behalf.
fn confirmed(req: &Request) -> bool {
    req.json_body().get("confirm").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// `history` — the journal, filtered the way the core filters it, not the way the window would.
fn history(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let mut args = vec!["log".to_string(), name];
    for (param, flag) in [("type", "--type"), ("path", "--path"), ("grep", "--grep"), ("content", "--content"), ("since", "--since"), ("until", "--until")] {
        if let Some(v) = req.param(param) {
            if !v.is_empty() {
                args.push(flag.to_string());
                args.push(v);
            }
        }
    }
    core_read(ctx, args)
}

/// `file` — one file as it was at one moment. This is the only route that hands over file
/// *content*, and the core decides what may leave the archive: it refuses nothing, but it marks
/// binary data as binary and never sends more than its cap.
fn project_file(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let path = match req.param("path") {
        Some(p) if !p.is_empty() => p,
        _ => return err(400, "path is required"),
    };
    let mut args = vec!["cat".to_string(), name, "--path".to_string(), path];
    if let Some(at) = req.param("at") {
        args.push("--at".into());
        args.push(at);
    }
    core_read(ctx, args)
}

fn project_why(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let mut args = vec!["why".to_string(), name];
    if let Some(p) = req.param("path") {
        if !p.is_empty() {
            args.push(p);
        }
    }
    if let Some(at) = req.param("at") {
        args.push("--at".into());
        args.push(at);
    }
    core_read(ctx, args)
}

fn project_blame(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let path = match req.param("path") {
        Some(p) if !p.is_empty() => p,
        _ => return err(400, "path is required"),
    };
    let mut args = vec!["blame".to_string(), name, path];
    if let Some(l) = req.param("limit") {
        args.push("--limit".into());
        args.push(l);
    }
    core_read(ctx, args)
}

/// `diff` — two moments, or one moment and what is on the disk now.
fn project_diff(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let from = match req.param("from") {
        Some(f) if !f.is_empty() => f,
        _ => return err(400, "from is required (the earlier moment)"),
    };
    let mut args = vec!["diff".to_string(), name, "--at".to_string(), from];
    match req.param("to") {
        Some(t) if !t.is_empty() => {
            args.push("--to".into());
            args.push(t);
        }
        _ => args.push("--current".into()),
    }
    core_read(ctx, args)
}

fn project_last_good(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    core_read(ctx, vec!["last-good".to_string(), name])
}

/// What a restore would do — asked with `restore --preview --json`, which writes nothing.
fn restore_preview(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let at = match req.str_field("at") {
        Some(a) => a,
        None => return err(400, "at is required"),
    };
    let mut args = vec!["restore".to_string(), name, "--at".to_string(), at, "--preview".to_string()];
    if let Some(to) = req.str_field("to") {
        args.push("--to".into());
        args.push(to);
    }
    if req.json_body().get("intoProject").and_then(|v| v.as_bool()).unwrap_or(false) {
        args.push("--into-project".into());
    }
    if req.json_body().get("clean").and_then(|v| v.as_bool()).unwrap_or(false) {
        args.push("--clean".into());
    }
    if req.json_body().get("missing").and_then(|v| v.as_bool()).unwrap_or(false) {
        args.push("--missing".into());
    }
    for p in req.str_list("paths") {
        args.push("--path".into());
        args.push(p);
    }
    core_read(ctx, args)
}

/// `check` — the integrity of the archive itself: blobs present, blobs readable, nothing dangling.
fn check(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let mut args = vec!["check".to_string()];
    if let Some(n) = req.param("name") {
        if !n.is_empty() {
            args.push(n);
        }
    }
    if req.param("deep").map(|v| v == "1" || v == "true").unwrap_or(false) {
        args.push("--deep".into());
    }
    core_read(ctx, args)
}

fn size_now(ctx: &Arc<Ctx>) -> Response {
    core_read(ctx, vec!["size".to_string()])
}

fn gc_now(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let mut args = vec!["gc".to_string(), "--dry-run".to_string()];
    if let Some(n) = req.param("name") {
        if !n.is_empty() {
            args.push(n);
        }
    }
    core_read(ctx, args)
}

fn audit_archive(ctx: &Arc<Ctx>) -> Response {
    // No --update here on purpose: a digest is a record of a moment the person chose to freeze,
    // and the window is not going to choose one for them.
    core_read(ctx, vec!["audit-archive".to_string()])
}

fn quarantine_list(ctx: &Arc<Ctx>) -> Response {
    core_read(ctx, vec!["quarantine".to_string()])
}

fn retention_get(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req_name(req) {
        Ok(n) => n,
        Err(r) => return r,
    };
    core_read(ctx, vec!["retention".to_string(), name])
}

fn recent_now(ctx: &Arc<Ctx>) -> Response {
    core_read(ctx, vec!["recent".to_string()])
}

/// Round 300: what the program has told the person, from the archive's own ledger. Real lines only —
/// the design's notification centre had no data behind it until the core started writing them.
fn notifications(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let mut args = vec!["notifications".to_string(), "--json".to_string()];
    if let Some(l) = req.param("limit") {
        if !l.is_empty() {
            args.push("--limit".to_string());
            args.push(l);
        }
    }
    if let Some(k) = req.param("kind") {
        if !k.is_empty() {
            args.push("--kind".to_string());
            args.push(k);
        }
    }
    if let Some(s) = req.param("since") {
        if !s.is_empty() {
            args.push("--since".to_string());
            args.push(s);
        }
    }
    core_read(ctx, args)
}

/// Every regular file under a folder, with the sha256 the window computes itself.
/// Used to prove the promise of a repair: nothing that already existed may change, and nothing may
/// disappear. The core is the writer; this is a second opinion that shares no code with it.
fn hash_tree(root: &Path) -> std::collections::BTreeMap<String, (u64, String)> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            let md = match std::fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                stack.push(p);
            } else if md.is_file() {
                let rel = p.strip_prefix(root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                let hash = pl::sha256_file(&p).unwrap_or_else(|_| "unreadable".into());
                out.insert(rel, (md.len(), hash));
            }
        }
    }
    out
}

/// "Give back the files that are missing" — `pl restore --missing`, run as a job, then checked here.
///
/// The check is the point. Before the core runs, every file in the project is hashed by this server;
/// afterwards they are hashed again. A repair may create; it may not overwrite and may not delete, so
/// any pre-existing file whose bytes changed, or any that vanished, is reported as a violation rather
/// than described as success.
fn repair_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let at = match req.str_field("at") {
        Some(a) => a,
        None => return err(400, "at is required"),
    };
    let into = req.json_body().get("intoProject").and_then(|v| v.as_bool()).unwrap_or(true);
    let to = req.str_field("to");
    if !into && to.is_none() {
        return err(400, "to is required when the repair does not write into the project");
    }
    let ctx2 = Arc::clone(ctx);
    let label = format!("Repair {name} at {at}");
    let id = ctx.jobs.start("repair", &label, move |h| {
        repair_job(&ctx2, &name, &at, into, to.as_deref(), h)
    });
    ok(json!({"job": id}))
}

fn repair_job(
    ctx: &Arc<Ctx>,
    name: &str,
    at: &str,
    into_project: bool,
    to: Option<&str>,
    h: &JobHandle,
) -> Result<Value, String> {
    let core = ctx.core();
    let root = match to {
        Some(t) => PathBuf::from(t),
        None => {
            // The project's own root, as the core reports it — never guessed from a name.
            let st = core.json(&["status".to_string(), name.to_string(), "--json".to_string()])?;
            let first = st.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
            match first.get("projectRoot").and_then(|v| v.as_str()) {
                Some(p) => PathBuf::from(p),
                None => return Err(format!("the core did not report a project root for {name}")),
            }
        }
    };
    let before = hash_tree(&root);
    h.line(&format!(
        "before: {} file(s) under {} — every one of them must be byte-identical afterwards",
        before.len(),
        root.display()
    ));
    // What the archive says existed at that moment, read before anything is written.
    let tree = core.json(&["tree".to_string(), name.to_string(), "--at".to_string(), at.to_string(), "--json".to_string()])?;
    let mut expected: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Value::Array(rows) = &tree {
        for r in rows {
            if r.get("kind").and_then(|v| v.as_str()).unwrap_or("file") != "file" {
                continue;
            }
            let p = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let hash = r.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string();
            expected.insert(p, hash);
        }
    }
    let mut args: Vec<String> = vec![
        "restore".into(),
        name.to_string(),
        "--at".into(),
        at.to_string(),
        "--missing".into(),
        "--yes".into(),
        "--json".into(),
    ];
    if into_project {
        args.push("--into-project".into());
    } else if let Some(t) = to {
        args.push("--to".into());
        args.push(t.to_string());
    }
    let (code, out) = jobs::run_streaming(&core, &args, h)?;
    let parsed: Value = serde_json::from_str(out.trim()).map_err(|e| {
        format!("the core exited {code} and its output was not the expected object: {e}")
    })?;
    let restored: Vec<String> = parsed
        .get("restoredPaths")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let after = hash_tree(&root);
    let mut overwritten: Vec<Value> = Vec::new();
    let mut deleted: Vec<Value> = Vec::new();
    for (rel, (len, hash)) in &before {
        match after.get(rel) {
            None => deleted.push(json!({"path": rel})),
            Some((len2, hash2)) => {
                if hash2 != hash || len2 != len {
                    overwritten.push(json!({"path": rel, "before": hash, "after": hash2}));
                }
            }
        }
    }
    let mut verified = 0usize;
    let mut mismatched: Vec<Value> = Vec::new();
    for rel in &restored {
        let p = root.join(rel);
        match (pl::sha256_file(&p), expected.get(rel)) {
            (Ok(actual), Some(exp)) if &actual == exp => verified += 1,
            (Ok(actual), Some(exp)) => mismatched.push(json!({"path": rel, "expected": exp, "actual": actual})),
            (Ok(_), None) => mismatched.push(json!({"path": rel, "why": "not in the tree at that moment"})),
            (Err(e), _) => mismatched.push(json!({"path": rel, "why": format!("cannot read it back: {e}")})),
        }
    }
    h.line(&format!(
        "after: {} created and verified byte for byte, {} pre-existing file(s) changed, {} deleted",
        verified,
        overwritten.len(),
        deleted.len()
    ));
    Ok(json!({
        "root": root.to_string_lossy(),
        "intoProject": into_project,
        "created": restored.len(),
        "verified": verified,
        "mismatched": mismatched,
        "presentBefore": before.len(),
        "overwritten": overwritten,
        "deletedByRepair": deleted,
        "coreClaim": {
            "restored": parsed.get("restored").cloned().unwrap_or(Value::Null),
            "skippedPresent": parsed.get("skippedPresent").cloned().unwrap_or(Value::Null),
            "failed": parsed.get("failed").cloned().unwrap_or(Value::Null),
            "bytes": parsed.get("bytes").cloned().unwrap_or(Value::Null),
        },
        "output": out,
        "exit": code,
    }))
}

fn config_all(ctx: &Arc<Ctx>) -> Response {
    core_read(ctx, vec!["config".to_string(), "get".to_string()])
}

fn project_note(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let text = req.str_field("text").unwrap_or_default();
    core_write(ctx, &["note".to_string(), name, text])
}

fn project_mark(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let label = req.str_field("label").filter(|s| !s.trim().is_empty());
    let mut args = vec!["mark".to_string(), name];
    if let Some(l) = label {
        args.push(l);
    } else {
        // No label given: `snap` is the same act with a name made from the moment.
        args[0] = "snap".to_string();
    }
    core_write(ctx, &args)
}

fn project_relink(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let path = match req.str_field("path") {
        Some(p) => p,
        None => return err(400, "path is required"),
    };
    if !confirmed(req) {
        return err(409, "this changes where the project points; the window must ask first");
    }
    core_write(ctx, &["relink".to_string(), name, path])
}

fn project_remove(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    if !confirmed(req) {
        return err(409, "this stops observing the folder; the window must ask first");
    }
    core_write(ctx, &["remove".to_string(), name])
}

fn project_apply_filters(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    core_write(ctx, &["apply-filters".to_string(), name])
}

fn retention_set(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let policy = match req.str_field("policy") {
        Some(p) if !p.trim().is_empty() => p,
        _ => return err(400, "policy is required, for example \"7d:all,30d:1/day\""),
    };
    // Storing a policy deletes nothing, so no confirmation is asked for here; applying one is a
    // different route and does ask.
    core_write(ctx, &["retention".to_string(), name, policy])
}

fn retention_apply(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    let policy = req.str_field("policy").unwrap_or_default();
    let stored = if policy.trim().is_empty() {
        // No policy in the request: use the one stored for this project, the same one the window
        // is showing. If there is none, the core says so in its own words.
        match ctx.core().json(&["retention".to_string(), name.clone(), "--json".to_string()]) {
            Ok(v) => v.get("policy").and_then(|p| p.as_str()).unwrap_or("").to_string(),
            Err(e) => return err(400, &e),
        }
    } else {
        policy
    };
    if stored.trim().is_empty() {
        return err(400, "no retention policy is stored for this project");
    }
    let dry = req.json_body().get("dryRun").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut args = vec!["prune".to_string(), name, "--policy".to_string(), stored, "--json".to_string()];
    if dry {
        args.push("--dry-run".into());
    } else {
        if !confirmed(req) {
            return err(409, "pruning deletes versions for good; the window must ask first");
        }
        args.push("--yes".into());
    }
    match ctx.core().json(&args) {
        Ok(v) => ok(v),
        Err(e) => err(400, &e),
    }
}

fn recover_now(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    if !confirmed(req) {
        return err(409, "recovery finishes or rolls back an interrupted prune; the window must ask first");
    }
    core_write(ctx, &["recover".to_string(), name])
}

/// `drill` — a rehearsal: restore into a temporary folder and compare byte for byte. It starts a
/// job because it reads every blob of the moment, and the window should stay alive while it does.
fn drill_start(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let name = match req.str_field("name") {
        Some(n) => n,
        None => return err(400, "name is required"),
    };
    if let Some(id) = ctx.jobs.running_of_kind("drill") {
        return err(409, &format!("a rehearsal is already running (job {id})"));
    }
    let ctx2 = Arc::clone(ctx);
    let label = format!("Rehearsal: restore {name} from the archive and compare");
    let id = ctx.jobs.start("drill", &label, move |h| {
        let core = ctx2.core();
        let args = vec!["drill".to_string(), name.clone(), "--json".to_string()];
        let (code, out) = jobs::run_streaming(&core, &args, h)?;
        if code != 0 {
            return Err(format!("the rehearsal exited {code}"));
        }
        Ok(pl::last_json(&out).unwrap_or(Value::Null))
    });
    ok(json!({"job": id}))
}

// ------------------------------------------------------------------------------------------
// The menu (round 299)
//
// The registry itself is `crate::menu`: one list, and the window and the macOS menu bar are both
// drawn from it. These two routes only *read* that list and *run* one entry of it. An id that is
// not in the table cannot be run through here — the menu is not a command runner, and this is the
// line that makes that true rather than promised.

fn menu_json(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let lang = req
        .param("lang")
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| ctx.ui_lang.lock().map(|l| l.clone()).unwrap_or_else(|_| "en".into()));
    let archive = ctx.archive_root().is_some();
    let shell = req.param("shell").as_deref() == Some("1");
    if req.param("plain").as_deref() == Some("1") {
        // The same list, for a reader that is not a browser: see `menu::plain`.
        let project = req.param("project").unwrap_or_default();
        return Response::text(
            crate::menu::plain(&lang, Some(project.as_str()), archive, !shell),
            "text/plain; charset=utf-8",
        );
    }
    let mut v = if shell {
        crate::menu::json_shell(&lang, archive)
    } else {
        let project = req.param("project").unwrap_or_default();
        crate::menu::json(&lang, Some(project.as_str()), archive)
    };
    if let Some(o) = v.as_object_mut() {
        o.insert("build".into(), build_info(ctx));
        o.insert("archive".into(), json!(ctx.archive_root().map(|p| p.display().to_string())));
    }
    ok(v)
}

fn menu_run(ctx: &Arc<Ctx>, req: &Request) -> Response {
    let id = match req.str_field("id") {
        Some(i) if !i.is_empty() => i,
        _ => return err(400, "id is required"),
    };
    let item = match crate::menu::find(&id) {
        Some(i) => i,
        None => {
            return err(
                400,
                &format!(
                    "no such menu entry: {id}. The menu is a fixed list; only its entries can be run \
                     through this route."
                ),
            )
        }
    };
    if item.place == crate::menu::Place::Shell {
        return err(400, &format!("{id} is the app shell's own act, not the server's"));
    }
    let project = req.str_field("project").filter(|p| !p.is_empty());
    let input = req.str_field("input").filter(|p| !p.is_empty());
    let confirmed = req
        .json_body()
        .get("confirm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if crate::menu::needs_input(item) && input.is_none() {
        return err(400, &format!("{id} needs a value first ({} )", item.input));
    }
    if item.kind == crate::menu::Kind::Confirm && !confirmed {
        return err(
            409,
            &format!("{id} changes what is stored; the window asks first and then sends confirm:true"),
        );
    }
    if item.kind == crate::menu::Kind::View || item.kind == crate::menu::Kind::Page {
        // Nothing to run: the window performs it through the flow it already had. The line is
        // logged so that "the menu did something" is auditable from the outside either way.
        menu_log(ctx, &format!("menu {id} -> the window ({})", item.kind.id()));
        return ok(json!({
            "id": id,
            "kind": item.kind.id(),
            "view": item.view,
            "note": item.note,
            "core": item.core,
            "ran": false,
        }));
    }
    if item.kind == crate::menu::Kind::Info {
        return ok(json!({
            "id": id,
            "kind": "info",
            "core": item.core,
            "note": item.note,
            "ran": false,
        }));
    }
    if !item.special.is_empty() {
        return match item.special {
            "diagnose" => diagnose_self(ctx, &id),
            other => err(500, &format!("the server has no implementation for '{other}'")),
        };
    }

    let archive = ctx.archive_root();
    let archive_str = archive.as_ref().and_then(|p| p.to_str());
    let argv = match crate::menu::substitute(item.argv, project.as_deref(), archive_str, input.as_deref()) {
        Ok(a) => a,
        Err(e) => return err(400, &e),
    };
    let core = ctx.core();
    let t0 = pl::now_ms();
    let (code, out, e) = core.run(&argv);
    let ms = pl::now_ms() - t0;
    menu_log(
        ctx,
        &format!("menu {id} ran `pl {}` -> exit {code} in {ms} ms", argv.join(" ")),
    );
    ok(json!({
        "id": id,
        "kind": item.kind.id(),
        "argv": argv,
        "core": item.core,
        "coreRun": format!("pl {}", argv.join(" ")),
        "exit": code,
        "stdout": out,
        "stderr": e,
        "ms": ms,
        "writes": crate::menu::writes(item),
    }))
}

/// The one entry the server performs itself: run the interface server's own diagnosis in a separate
/// process, exactly the way the macOS menu does. It writes diagnose.txt beside the log and prints
/// its verdict, so the window can show the same words the shell shows.
fn diagnose_self(ctx: &Arc<Ctx>, id: &str) -> Response {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return err(500, &format!("cannot find this program's own path: {e}")),
    };
    let dir = ctx
        .daemon_log
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&dir);
    let out = Command::new(&exe)
        .arg("--diagnose")
        .arg("--log-dir")
        .arg(&dir)
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            let report = dir.join("diagnose.txt");
            let verdict = text
                .lines()
                .find(|l| l.starts_with("verdict:"))
                .unwrap_or("verdict: (none)")
                .to_string();
            menu_log(ctx, &format!("menu {id} ran the diagnosis in a separate process: {verdict}"));
            ok(json!({
                "id": id,
                "kind": "run",
                "exit": o.status.code().unwrap_or(-1),
                "stdout": text,
                "stderr": "",
                "core": "projectlife-ui --diagnose",
                "coreRun": format!("{} --diagnose --log-dir {}", exe.display(), dir.display()),
                "report": report.display().to_string(),
                "verdict": verdict,
            }))
        }
        Err(e) => err(500, &format!("cannot run the diagnosis: {e}")),
    }
}

/// Every menu run lands in the same log as everything else the app does: who asked, what ran, what
/// it answered. A menu that does nothing silently would be indistinguishable from a working one.
fn menu_log(ctx: &Arc<Ctx>, line: &str) {
    app_log(ctx, line);
}

fn app_log(ctx: &Arc<Ctx>, line: &str) {
    let stamp = crate::listen::stamp(pl::now_ms());
    if let Some(dir) = ctx.daemon_log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ctx.daemon_log)
    {
        use std::io::Write;
        let _ = writeln!(f, "{stamp} projectlife-ui: {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_iso_form_the_window_sends_round_trips() {
        for ms in [0i64, 1_791_265_573_488, 1_791_265_573_000, 4_102_444_800_000] {
            let iso = ms_to_iso(ms);
            assert_eq!(crate::pl::parse_iso_utc(&iso), Some(ms), "round trip failed for {ms} ({iso})");
        }
    }

    #[test]
    fn a_state_is_named_the_way_the_design_names_it() {
        assert_eq!(state_label("active"), "protected");
        assert_eq!(state_label("paused"), "paused");
        assert_eq!(state_label("path_missing"), "path missing");
        assert_eq!(state_label("something-new"), "unknown");
    }

    // ------------------------------------------------------------------ round 298: which build is
    // this? The owner ran a bundle that had been replaced hours earlier and nothing said so. The
    // answer has to come from the files, not from a name someone typed.

    fn ctx_with(bin: PathBuf) -> Arc<Ctx> {
        Arc::new(Ctx {
            bin,
            home: None,
            archive: Mutex::new(None),
            token: "t".into(),
            jobs: Registry::new(),
            daemon: Mutex::new(None),
            daemon_log: std::env::temp_dir().join("pl-build-info-test.log"),
            native: false,
            ui_lang: Mutex::new("en".into()),
            self_stopped_ms: Mutex::new(None),
        })
    }

    /// The reported hash must be the hash of the file that is actually there. A test that compared
    /// the number with a constant would only pin the constant.
    #[test]
    fn the_build_identity_is_the_hash_of_the_file_on_disk() {
        let dir = std::env::temp_dir().join(format!("pl-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake_core = dir.join("projectlife");
        let content = b"not really a core, but it hashes like one\n";
        std::fs::write(&fake_core, content).unwrap();
        let expected = pl::sha256_file(&fake_core).unwrap();

        let ctx = ctx_with(fake_core.clone());
        let b = build_info(&ctx);
        assert_eq!(b["core"]["sha256"].as_str(), Some(expected.as_str()), "{b}");
        assert_eq!(b["core"]["bytes"].as_u64(), Some(content.len() as u64), "{b}");
        assert_eq!(b["core"]["short"].as_str(), Some(&expected[..8]), "{b}");
        assert_eq!(b["appVersion"].as_str(), Some(env!("CARGO_PKG_VERSION")), "{b}");
        assert!(
            b["short"].as_str().unwrap().contains(&expected[..8]),
            "the one-line identity carries the core hash: {b}"
        );
        assert!(
            b["check"].as_str().unwrap().contains("shasum -a 256"),
            "the window tells the reader how to check it independently: {b}"
        );

        // Replace the file: the next answer is about the new bytes, not the old ones.
        std::fs::write(&fake_core, b"a different build entirely\n").unwrap();
        let changed = pl::sha256_file(&fake_core).unwrap();
        assert_ne!(changed, expected, "the fixture must really change");
        let b2 = build_info(&ctx);
        assert_eq!(
            b2["core"]["sha256"].as_str(),
            Some(changed.as_str()),
            "a replaced binary must be re-hashed, not reported from memory: {b2}"
        );

        // The page the server serves is part of the identity too.
        assert_eq!(
            b2["page"]["sha256"].as_str(),
            Some(pl::sha256_bytes(crate::assets::APP_JS.as_bytes()).as_str()),
            "{b2}"
        );
        assert_eq!(b2["page"]["bytes"].as_u64(), Some(crate::assets::APP_JS.len() as u64), "{b2}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------ round 302: who made this,
    // and what it is for. The owner asked for his name and address to be written wherever the
    // program describes itself — and the risk in that request is the usual one: a name typed into a
    // dozen files drifts, and the drifted copy is the one a user reads. The app therefore *asks* the
    // core, and these two tests are the proof that it asks rather than remembers. The second is the
    // control: a core that cannot answer must be reported, not papered over with a plausible name.

    #[cfg(unix)]
    fn fake_core(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("projectlife");
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// The name the page shows is the core's answer, byte for byte — not a string this crate holds.
    /// The fake core answers with a name that appears nowhere in the app's own source.
    #[cfg(unix)]
    #[test]
    fn the_program_block_is_the_cores_own_answer() {
        let dir = std::env::temp_dir().join(format!("pl-brand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let core = fake_core(
            &dir,
            "#!/bin/sh\nprintf '%s\\n' '{\"product\":\"Project Life\",\"author\":\"Someone Else\",\"authorEmail\":\"nobody@example.invalid\",\"by\":\"Someone Else <nobody@example.invalid>\",\"whatItIs\":\"a sentence\",\"whatItIsRu\":\"предложение\",\"licence\":\"MIT\",\"copyright\":\"Copyright (c) 2026 Someone Else\"}'\n",
        );
        let ctx = ctx_with(core);
        let b = brand(&ctx);
        assert_eq!(b["author"].as_str(), Some("Someone Else"), "{b}");
        assert_eq!(b["authorEmail"].as_str(), Some("nobody@example.invalid"), "{b}");
        assert_eq!(b["whatItIs"].as_str(), Some("a sentence"), "{b}");
        assert!(b["unavailable"].is_null(), "a core that answered is not unavailable: {b}");

        // …and the bootstrap the page polls carries it, so the About screen cannot show one thing
        // while the licence says another.
        let body: Value = serde_json::from_slice(&bootstrap(&ctx).body).unwrap();
        assert_eq!(body["app"]["author"].as_str(), Some("Someone Else"), "{body}");
        assert_eq!(body["app"]["authorEmail"].as_str(), Some("nobody@example.invalid"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The control: with a core that refuses, the block says so and carries no name at all. Without
    /// this, the test above would also pass on a program that always prints the same invented name.
    #[cfg(unix)]
    #[test]
    fn a_core_that_cannot_answer_is_reported_and_no_name_is_invented() {
        let dir = std::env::temp_dir().join(format!("pl-brand-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let core = fake_core(&dir, "#!/bin/sh\necho 'the core is not here' >&2\nexit 1\n");
        let ctx = ctx_with(core);
        let b = brand(&ctx);
        assert_eq!(b["unavailable"].as_bool(), Some(true), "{b}");
        assert!(b["why"].as_str().unwrap().contains("version --json"), "{b}");
        assert!(b["author"].is_null(), "no name may be invented: {b}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A core binary that is not there is reported as missing, not as an empty hash: an absent file
    /// is not a build.
    #[test]
    fn a_missing_core_is_reported_and_not_invented() {
        let ctx = ctx_with(PathBuf::from("/nonexistent/projectlife"));
        let b = build_info(&ctx);
        assert!(b["core"]["error"].is_string(), "{b}");
        assert!(b["core"]["sha256"].is_null(), "{b}");
        assert!(b["short"].as_str().unwrap().contains("core ?"), "{b}");
    }

    // ------------------------------------------------------------------ round 296: does the
    // window claim protection it does not have? On 2026-10-06 it did: the daemon was alive, so the
    // window said "Protected", while the daemon was writing nothing at all because the archive had
    // been wrongly declared full.

    fn beat(mode: &str, fresh: bool, age_ms: i64, storage: Value) -> Value {
        json!({"mode": mode, "fresh": fresh, "ageMs": age_ms, "maxAgeMs": 60_000,
               "intervalMs": 5000, "storage": storage})
    }

    fn storage(state: &str, stop: bool, warn: bool, reason: &str) -> Value {
        json!({"state": state, "stop": stop, "warn": warn, "reason": reason})
    }

    #[test]
    fn a_full_archive_is_not_protection_even_with_a_live_daemon() {
        let hb = beat(
            "daemon",
            true,
            300,
            storage("full", true, false, "free space 300.0 MB is below the stop threshold 500.0 MB"),
        );
        let p = protection(Some(&hb), true, None);
        assert_eq!(p["state"], "paused_full");
        assert_eq!(p["protected"], false, "a live process that writes nothing protects nothing");
        assert!(p["reason"].as_str().unwrap().contains("300.0 MB"), "the numbers survive into the window");
    }

    #[test]
    fn a_daemon_that_stopped_reporting_is_not_protection() {
        let hb = beat("daemon (stalled)", false, 900_000, storage("ok", false, false, "plenty"));
        let p = protection(Some(&hb), true, None);
        assert_eq!(p["state"], "stale");
        assert_eq!(p["protected"], false);
        assert!(p["reason"].as_str().unwrap().contains("900 s"), "{}", p["reason"]);
    }

    #[test]
    fn a_fresh_heartbeat_from_a_live_observer_is_protection() {
        let hb = beat("daemon", true, 1200, storage("ok", false, false, "plenty of room"));
        let p = protection(Some(&hb), true, None);
        assert_eq!(p["state"], "protected");
        assert_eq!(p["protected"], true);
    }

    #[test]
    fn low_space_is_a_warning_and_still_protection_because_writing_continues() {
        let hb = beat("daemon", true, 1200, storage("warn", false, true, "free space 1.8 GB is below the warning threshold 5.0 GB"));
        let p = protection(Some(&hb), true, None);
        assert_eq!(p["state"], "protected_low_space");
        assert_eq!(p["protected"], true);
        assert!(p["reason"].as_str().unwrap().contains("1.8 GB"), "{}", p["reason"]);
    }

    #[test]
    fn a_heartbeat_from_just_before_the_window_stopped_is_not_protection() {
        let hb = beat("external timer", true, 200, storage("ok", false, false, "plenty of room"));
        let p = protection(Some(&hb), false, Some(1_791_265_573_488));
        assert_eq!(p["state"], "stopped");
        assert_eq!(p["protected"], false);
    }

    #[test]
    fn no_answer_from_the_core_is_admitted_not_assumed() {
        let p = protection(None, true, None);
        assert_eq!(p["state"], "unknown");
        assert_eq!(p["protected"], false);
        assert!(p["reason"].as_str().unwrap().contains("did not answer"), "{}", p["reason"]);
    }

    #[test]
    fn config_defaults_are_what_the_core_ships() {
        let cfg = read_config(None);
        assert_eq!(cfg["intervalSeconds"], 5);
        assert_eq!(cfg["notifications"], true);
    }

    #[test]
    fn a_token_is_required_and_compared_exactly() {
        let ctx = Ctx {
            bin: std::path::PathBuf::from("/bin/true"),
            home: None,
            archive: Mutex::new(None),
            token: "secret".into(),
            jobs: crate::jobs::Registry::new(),
            daemon: Mutex::new(None),
            daemon_log: std::path::PathBuf::from("/tmp/pl-app-test.log"),
            native: false,
            ui_lang: Mutex::new("en".into()),
            self_stopped_ms: Mutex::new(None),
        };
        let req = |q: &str, hdr: bool| crate::http::Request {
            method: "GET".into(),
            path: "/api/bootstrap".into(),
            query: [("token".to_string(), q.to_string())].into_iter().collect(),
            headers: if hdr { [("x-pl-token".to_string(), q.to_string())].into_iter().collect() }
                     else { Default::default() },
            body: Vec::new(),
        };
        assert!(authorized(&ctx, &req("secret", false)));
        assert!(authorized(&ctx, &req("secret", true)));
        assert!(!authorized(&ctx, &req("secre", false)), "a prefix is not the token");
        assert!(!authorized(&ctx, &req("secretx", false)), "a longer string is not the token");
        assert!(!authorized(&ctx, &req("", false)));
    }
}
