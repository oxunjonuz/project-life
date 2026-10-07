//! Command line interface. Hand-rolled argument parsing keeps the binary free of dependencies and
//! makes the exit-code contract (0 ok, 1 error, 2 partial, 3 cancelled) explicit everywhere.

use crate::archive::{self, iso_ms, Archive, Config, Project};
use crate::daemon;
use crate::detect;
use crate::doctor::{self, ERROR, OK, WARN};
use crate::events::{self, Ev};
use crate::filters::FilterConfig;
use crate::lifecycle;
use crate::profiles;
use crate::restore::{self, RestoreOptions};
use crate::scan::{self, ScanOptions};
use crate::store::Store;
use crate::util;
use crate::VERSION;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERR: i32 = 1;
pub const EXIT_PARTIAL: i32 = 2;
pub const EXIT_CANCELLED: i32 = 3;

struct Args {
    positional: Vec<String>,
    flags: HashMap<String, String>,
    multi: HashMap<String, Vec<String>>,
    json: bool,
}

impl Args {
    fn has(&self, k: &str) -> bool {
        self.flags.contains_key(k)
    }
    fn val(&self, k: &str) -> Option<String> {
        self.flags.get(k).cloned()
    }
    fn list(&self, k: &str) -> Vec<String> {
        self.multi.get(k).cloned().unwrap_or_default()
    }
}

fn value_flags() -> &'static [&'static str] {
    &[
        "archive", "lang", "name", "new", "profile", "at", "to", "path", "mark", "since", "until", "type",
        "out", "export-first", "before", "from", "into", "interval", "deep-interval",
        // Round 291 — universal mode.
        "preset", "edit-add", "edit-remove",
        // Round 292 — retention policy, scheduler checks and the small daily commands.
        "policy", "max-age", "sort", "grep", "content", "for", "top", "limit", "restored", "kind",
    ]
}

fn parse_args(argv: &[String]) -> Args {
    let mut a = Args { positional: Vec::new(), flags: HashMap::new(), multi: HashMap::new(), json: false };
    let vf = value_flags();
    let mut i = 0;
    while i < argv.len() {
        let t = &argv[i];
        if let Some(rest) = t.strip_prefix("--") {
            let (key, inline) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if key == "json" {
                a.json = true;
                i += 1;
                continue;
            }
            let value = if let Some(v) = inline {
                v
            } else if vf.contains(&key.as_str()) && i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                i += 1;
                argv[i].clone()
            } else {
                String::new()
            };
            if key == "path" || key == "edit-add" || key == "edit-remove" {
                a.multi.entry(key.clone()).or_default().push(value.clone());
            }
            a.flags.insert(key, value);
            i += 1;
        } else {
            a.positional.push(t.clone());
            i += 1;
        }
    }
    a
}

fn stdin_is_tty() -> bool {
    util::stdin_is_tty()
}

/// Ask for confirmation. Without a TTY or `--yes` the operation is refused, never assumed.
fn confirm(prompt: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !stdin_is_tty() {
        println!("{prompt}");
        println!("(no interactive terminal: re-run with --yes to confirm)");
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes" | "д" | "да")
}

fn confirm_name(prompt: &str, expected: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !stdin_is_tty() {
        println!("{prompt}");
        println!("(no interactive terminal: re-run with --yes to confirm)");
        return false;
    }
    print!("{prompt} (type the project name, '{expected}'): ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    line.trim() == expected
}

fn open_archive(a: &Args) -> Result<Archive, String> {
    let root = archive::resolve_archive_root(a.val("archive").as_deref())?;
    Archive::open(&root)
}

fn parse_moment(a: &Args, project: &Project, journal: &[Ev]) -> Result<(i64, String), String> {
    if a.has("last-good") {
        let (ts, why) = restore::last_good(journal, project)
            .ok_or_else(|| "no mass events in this project: use --at <moment>".to_string())?;
        return Ok((ts, format!("last-good ({why})")));
    }
    if let Some(mark) = a.val("mark") {
        let ev = journal
            .iter()
            .filter(|e| events::event_type(e) == "mark" && events::get_str(e, "label").as_deref() == Some(mark.as_str()))
            .max_by_key(|e| events::ts_of(e))
            .ok_or_else(|| format!("mark not found: {mark}"))?;
        return Ok((events::ts_of(ev), format!("mark \"{mark}\"")));
    }
    let spec = a.val("at").ok_or_else(|| "one of --at, --mark, --last-good is required".to_string())?;
    if let Some(seq) = spec.strip_prefix("seq:") {
        let n: u64 = seq.trim().parse().map_err(|_| format!("bad seq: {spec}"))?;
        let ev = journal
            .iter()
            .filter(|e| events::seq_of(e) == n)
            .max_by_key(|e| events::seq_of(e))
            .ok_or_else(|| format!("no event with seq {n}"))?;
        return Ok((events::ts_of(ev), format!("seq:{n}")));
    }
    match util::parse_at(&spec, util::now_ms()) {
        Ok(ms) => Ok((ms, spec.clone())),
        Err(e) => Err(format!("cannot parse --at {spec}: {e}")),
    }
}

fn print_plan(plan: &restore::RestorePlan, project: &Project, into_project: bool) {
    println!("Project:      {}", project.name);
    println!("Moment:       {} ({})", util::fmt_local(plan.moment), plan.at_label);
    println!("Target:       {}", plan.target.display());
    let mode = if plan.missing {
        "repair: only what is missing (nothing is overwritten, nothing is deleted)"
    } else if into_project {
        "into the project folder"
    } else {
        "separate folder"
    };
    println!("Mode:         {mode}");
    println!("Files:        {} to create, {} to overwrite", plan.create, plan.overwrite);
    if plan.missing {
        println!("Left alone:   {} files already on disk", plan.present);
    }
    if plan.delete_extra > 0 {
        println!("To delete:    {} files (--clean)", plan.delete_extra);
    }
    println!("Missing blobs: {}", plan.missing_blobs.len());
    println!("Size:         {}", util::human_size(plan.bytes));
    if !plan.symlinks.is_empty() {
        println!("Symlinks:     {} to recreate", plan.symlinks.len());
    }
    for w in &plan.warnings {
        println!("WARNING: {w}");
    }
}

pub fn run(argv: Vec<String>) -> i32 {
    let a = parse_args(&argv);
    let cmd = match a.positional.first() {
        Some(c) => c.clone(),
        None => {
            // A long option with no command: `pl --version` and `pl --help` are the two things a
            // person types before anything else. Until round 302 both printed the usage and exited 1
            // — the answer was printed and the exit code called it a failure. Found while putting the
            // author line into `version`, which is exactly the command that was unreachable.
            if a.flags.contains_key("help") {
                "help".to_string()
            } else if a.flags.contains_key("version") {
                "version".to_string()
            } else {
                let mut unknown: Vec<String> = a.flags.keys().map(|k| format!("--{k}")).collect();
                unknown.sort();
                if !unknown.is_empty() {
                    eprintln!(
                        "error: no command given (unrecognised option(s): {})",
                        unknown.join(", ")
                    );
                }
                print_usage();
                return EXIT_ERR;
            }
        }
    };
    let rest: Vec<String> = a.positional.iter().skip(1).cloned().collect();
    let result = dispatch(&cmd, &rest, &a);
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_ERR
        }
    }
}

fn print_usage() {
    println!("{} {VERSION} — a local flight recorder for project file history", crate::brand::PRODUCT);
    println!("{}", crate::brand::WHAT_IT_IS);
    println!("By {} · {} licence", crate::brand::by(), crate::brand::LICENCE);
    println!();
    println!("Usage: projectlife <command> [options]");
    println!("       (alias: pl)");
    println!();
    println!("Archive:   init-archive <path> | archive-move <new-path> | audit-archive [--update]");
    println!("Projects:  add <path> [--name N] [--profile source|all|ask] [--yes] [--no-initial]");
    println!("           add <path> --preset auto|office|developer|designer|writer|photographer|");
    println!("               video|3d|data|audio|devops|custom [--yes] [--edit-add EXT] [--edit-remove EXT]");
    println!("           detect <path> [--json] | presets");
    println!("           list | status [project] [--skipped] [--risk] | pause | resume | remove");
    println!("           relink <project> <new-path> | note <project> \"text\"");
    println!("History:   log <project> [--path P] [--since T] [--until T] [--type ...]");
    println!("           tree <project> --at T | diff <project> --at T [--to T2|--current]");
    println!("Restore:   restore <project> --at T|--mark M|--last-good [--path P] [--to DIR]");
    println!("           [--into-project [--clean]] [--missing] [--preview] [--yes]");
    println!("           panic <project> [--to DIR] | last-good <project> | mark <project> \"label\"");
    println!("Answers:   why <project> <path> | why <project> --at T");
    println!("Lifecycle: prune <project> --before T [--dry-run] | export <project> --out DIR [--pack]");
    println!("           export-and-prune | import <export-dir> [--new NAME] | archive-delete");
    println!("           recover <project>   # finish or roll back an interrupted prune");
    println!("Checks:    check [project] [--deep] [--fix] | rebuild-cache");
    println!("           doctor [--json] [--fix-lock [--force]]");
    println!("Running:   scan-once [--all|<project>] [--skipped] | daemon run [--no-watch]");
    println!("Scoped:    partial-pass <project> <path>...   # exactly the pass one notification runs");
    println!("           daemon install|start|stop|status   # --no-watch: periodic pass only");
    println!("Promise:   drill <project> [--json]");
    println!("Daily:     recent [--limit N] | since <project> [--at T] [--path P] [--stat]");
    println!("           snap <project> [label] | undo <project> | suggest [project] | blame <project> <path>");
    println!("           cat <project> --path P [--at T] [--out FILE] | size [--top N] | watch <project> [--for S]");
    println!("           list [--sort name|size|age|risk] | status [--compact] | log <project> [--grep S | --content S]");
    println!("           open <project> [--archive] [--restored DIR] [--launch] | gc [--dry-run] [project]");
    println!("           prompt [--space] | notifications [--limit N] [--kind K] [--since T]");
    println!("           notify test | completion <bash|zsh|fish> [--install]");
    println!("Retention: retention <project> [<policy>|apply|clear]");
    println!("           prune <project> --policy \"7d:all,30d:1/day,365d:1/month\" [--dry-run] [--yes]");
    println!("Schedulers: heartbeat-check [--max-age S] | healthcheck [--strict]");
    println!("Agents:    mcp [info|tools]  (the server itself: pl-mcp --archive <path>)");
    println!("Other:     config get|set <key> [value] | version");
    println!();
    println!("Global: --archive <path> --json --yes --lang <en|ru>");
    println!("Exit codes: 0 ok, 1 error, 2 partial success, 3 cancelled");
}

fn dispatch(cmd: &str, rest: &[String], a: &Args) -> Result<i32, String> {
    match cmd {
        "version" | "--version" | "-V" => {
            // Who made it and what it is for belongs in the one command a person runs to ask
            // "what is this?". The values come from `brand` — the same module the desktop shells
            // read through this command, so there is exactly one place to change a name.
            use crate::brand as b;
            if a.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "product": b::PRODUCT,
                        "version": VERSION,
                        "schema": crate::SCHEMA_VERSION,
                        "author": b::AUTHOR,
                        "authorEmail": b::AUTHOR_EMAIL,
                        "by": b::by(),
                        "whatItIs": b::WHAT_IT_IS,
                        "whatItIsRu": b::WHAT_IT_IS_RU,
                        "tagline": b::TAGLINE,
                        "taglineRu": b::TAGLINE_RU,
                        "answers": b::ANSWERS,
                        "licence": b::LICENCE,
                        "copyright": b::COPYRIGHT,
                    })
                );
                return Ok(EXIT_OK);
            }
            println!("{} {VERSION} (schema {})", b::PRODUCT, crate::SCHEMA_VERSION);
            println!("{}", b::WHAT_IT_IS);
            println!("By {} · {} licence", b::by(), b::LICENCE);
            println!("{}", b::COPYRIGHT);
            Ok(EXIT_OK)
        }
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(EXIT_OK)
        }
        "init-archive" => cmd_init_archive(rest, a),
        "archive-move" => cmd_archive_move(rest, a),
        "add" => cmd_add(rest, a),
        "detect" => cmd_detect(rest, a),
        "presets" => cmd_presets(a),
        "list" => cmd_list(a),
        "status" => cmd_status(rest, a),
        "pause" | "resume" | "remove" | "detach" => cmd_state_change(cmd, rest, a),
        "relink" => cmd_relink(rest, a),
        "note" => cmd_note(rest, a),
        "log" | "timeline" => cmd_log(rest, a, cmd == "timeline"),
        "tree" => cmd_tree(rest, a),
        "diff" => cmd_diff(rest, a),
        "restore" | "export-file" => cmd_restore(rest, a, cmd == "export-file"),
        "mark" => cmd_mark(rest, a),
        "last-good" => cmd_last_good(rest, a),
        "panic" => cmd_panic(rest, a),
        "why" => cmd_why(rest, a),
        "apply-filters" => cmd_apply_filters(rest, a),
        "prune" => cmd_prune(rest, a),
        "export" => cmd_export(rest, a),
        "export-and-prune" => cmd_export_and_prune(rest, a),
        "import" => cmd_import(rest, a),
        "archive-delete" => cmd_archive_delete(rest, a),
        "check" | "verify" => cmd_check(rest, a, cmd == "verify"),
        "rebuild-cache" => cmd_rebuild_cache(rest, a),
        "audit-archive" => cmd_audit_archive(a),
        "quarantine" => cmd_quarantine(rest, a),
        "scan-once" => cmd_scan_once(rest, a),
        "partial-pass" => cmd_partial_pass(rest, a),
        "drill" => cmd_drill(rest, a),
        "daemon" => cmd_daemon(rest, a),
        "config" => cmd_config(rest, a),
        "doctor" => cmd_doctor(a),
        "recover" => cmd_recover(rest, a),
        // Round 292 — the daily surface. Read-only unless the name says otherwise.
        "recent" | "r" => cmd_recent(a),
        "snap" => cmd_snap(rest, a),
        "undo" => cmd_undo(rest, a),
        "watch" => cmd_watch(rest, a),
        "open" => cmd_open(rest, a),
        "since" => cmd_since(rest, a),
        "cat" => cmd_cat(rest, a),
        "size" | "du" => cmd_size(a),
        "blame" => cmd_blame(rest, a),
        "suggest" => cmd_suggest(rest, a),
        "prompt" => cmd_prompt(a),
        "gc" => cmd_gc(rest, a),
        "notify" => cmd_notify(rest, a),
        "notifications" => cmd_notifications(rest, a),
        "completion" => cmd_completion(rest, a),
        "retention" => cmd_retention(rest, a),
        "heartbeat-check" => cmd_heartbeat_check(a),
        "healthcheck" => cmd_healthcheck(a),
        "mcp" => cmd_mcp(rest, a),
        other => Err(format!("unknown command: {other} (run `projectlife help`)")),
    }
}

fn cmd_init_archive(rest: &[String], a: &Args) -> Result<i32, String> {
    let path = rest
        .first()
        .cloned()
        .or_else(|| a.val("archive"))
        .ok_or_else(|| "usage: projectlife init-archive <path>".to_string())?;
    let root = PathBuf::from(&path);
    if Archive::looks_like_archive(&root) {
        println!("archive already present: {}", root.display());
    } else {
        let arch = Archive::create(&root)?;
        println!("archive created: {}", arch.root.display());
    }
    archive::save_location(&root).map_err(|e| format!("cannot save the archive location: {e}"))?;
    println!("location saved: {}", archive::location_file().display());
    Ok(EXIT_OK)
}

fn cmd_archive_move(rest: &[String], a: &Args) -> Result<i32, String> {
    let new_path = rest.first().ok_or_else(|| "usage: projectlife archive-move <new-path>".to_string())?;
    let root = PathBuf::from(new_path);
    if !Archive::looks_like_archive(&root) {
        return Err(format!(
            "{} does not look like an archive. Copy the archive folder there first, then re-run.",
            root.display()
        ));
    }
    archive::save_location(&root).map_err(|e| e.to_string())?;
    if let Ok(old) = archive::resolve_archive_root(None) {
        println!("archive location changed: {} -> {}", old.display(), root.display());
    } else {
        println!("archive location set: {}", root.display());
    }
    let _ = a;
    Ok(EXIT_OK)
}

// -------------------------------------------------------------------------------------------
// Round 291 — universal mode: presets and auto-detection (SPEC §6.17)
// -------------------------------------------------------------------------------------------

/// Print a detection report the way a person can act on it.
fn print_detection(d: &detect::Detection) {
    println!("Scanning {}...", d.root);
    println!();
    match d.best() {
        Some(b) if !d.suggest_custom => {
            println!("Detected profile: {} / {}", b.id, b.display_name);
            println!("Confidence: {}%", b.confidence);
        }
        Some(b) => {
            println!("Detected profile: none convincing (best: {} at {}%)", b.id, b.confidence);
            println!("Suggestion: custom — name the extensions yourself");
        }
        None => println!("Detected profile: none (no files a profile recognises)"),
    }
    if !d.extension_counts.is_empty() {
        println!();
        println!("Found extensions:");
        let mut v: Vec<(&String, &usize)> = d.extension_counts.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (ext, n) in v.iter().take(12) {
            println!("  {:<12} {} {}", ext, n, if **n == 1 { "file" } else { "files" });
        }
        if v.len() > 12 {
            println!("  … and {} more extensions", v.len() - 12);
        }
    }
    if !d.markers.is_empty() {
        println!("Found markers: {}", d.markers.join(", "));
    }
    if !d.folder_hints.is_empty() {
        println!("Folder hints:  {}", d.folder_hints.join(", "));
    }
    if !d.include_globs.is_empty() {
        println!();
        println!("Suggested include rules:");
        for chunk in d.include_globs.chunks(6) {
            println!("  {}", chunk.join(" "));
        }
        println!("Auto-exclude:");
        for chunk in d.exclude_globs.chunks(6) {
            println!("  {}", chunk.join(" "));
        }
        println!("Max file size: {}", util::human_size(d.max_file_size_kb * 1024));
        println!(
            "Estimated archive size: {} in {} files",
            util::human_size(d.estimated_archive_bytes),
            d.estimated_files
        );
    }
    if !d.unknown_extensions.is_empty() {
        println!();
        println!("Unknown extensions (never auto-included):");
        for (e, n) in d.unknown_extensions.iter().take(8) {
            println!("  {:<12} {} {}", e, n, if *n == 1 { "file" } else { "files" });
        }
    }
    if !d.safety_excluded.is_empty() {
        println!();
        println!("Excluded by the hard-coded safety rules ({}):", d.safety_excluded.len());
        for (p, r, rule) in d.safety_excluded.iter().take(8) {
            println!("  {p}   ({r}: {rule})");
        }
        if d.safety_excluded.len() > 8 {
            println!("  … and {} more", d.safety_excluded.len() - 8);
        }
    }
    for w in &d.warnings {
        println!("warning: {w}");
    }
    if d.unsized_files > 0 {
        println!(
            "note: {} files were counted without reading their size (extensions no profile claims, and not media or archives)",
            d.unsized_files
        );
    }
    println!();
    println!(
        "Scanned {} entries in {} ms ({}) — metadata only, no file was opened.",
        d.entries,
        d.elapsed_ms,
        if d.truncated { "partial" } else { "complete" }
    );
}

fn cmd_detect(rest: &[String], a: &Args) -> Result<i32, String> {
    let path = rest.first().ok_or_else(|| "usage: projectlife detect <path> [--json]".to_string())?;
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("not a folder: {}", root.display()));
    }
    // SPEC §6.17 §6: a system location is never a candidate, and saying so is better than printing
    // a confident suggestion for it.
    if let Some(pre) = profiles::system_root(&root) {
        return Err(format!(
            "{} is a system location ({pre}) — Project Life never protects one of those",
            root.display()
        ));
    }
    let d = detect::detect_profile(&root);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d.to_json()).unwrap_or_default());
    } else {
        print_detection(&d);
    }
    Ok(EXIT_OK)
}

fn cmd_presets(a: &Args) -> Result<i32, String> {
    if a.json {
        let arr: Vec<Value> = profiles::PROFILES
            .iter()
            .map(|p| {
                let (include, left_out) = profiles::include_globs(p);
                json!({
                    "id": p.id, "displayName": p.display_name,
                    "extensions": p.extensions, "markers": p.markers, "folderHints": p.folder_hints,
                    "defaultMaxFileSizeKb": p.default_max_file_size_kb,
                    "binaryPolicy": p.binary_policy, "mediaPolicy": p.media_policy,
                    "includeAfterPolicy": include, "mediaNotAutoIncluded": left_out,
                    "exclude": profiles::exclude_globs(p),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    println!("{:<13} {:<28} {:>12}  media", "PRESET", "NAME", "MAX SIZE");
    for p in profiles::PROFILES {
        println!(
            "{:<13} {:<28} {:>12}  {}",
            p.id,
            p.display_name,
            util::human_size(p.default_max_file_size_kb * 1024),
            p.media_policy
        );
    }
    println!("{:<13} {:<28} {:>12}  {}", "custom", "Custom (you name the extensions)", "-", "-");
    println!();
    println!("Every profile except `custom` leaves secrets, system paths, dependencies and temporary");
    println!("files out whatever it says — those rules are hard-coded (SPEC §6.17).");
    Ok(EXIT_OK)
}

/// What `--preset` decided, before it is written into `project.json`.
struct PresetPlan {
    id: String,
    display_name: String,
    /// `auto` (detected and confirmed) or `manual` (named on the command line).
    source: String,
    confidence: u32,
    include: Vec<String>,
    exclude: Vec<String>,
    max_kb: u64,
    media_left_out: Vec<String>,
    preset_modified: bool,
    evidence: Value,
    include_secrets: bool,
    include_large_files: bool,
}

impl PresetPlan {
    fn settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("filterMode".into(), Value::from("allow"));
        m.insert("include".into(), Value::from(self.include.clone()));
        m.insert("exclude".into(), Value::from(self.exclude.clone()));
        m.insert("maxFileSizeKb".into(), Value::from(self.max_kb));
        m.insert("presetId".into(), Value::from(self.id.clone()));
        if self.include_secrets {
            m.insert("includeSecrets".into(), Value::from(true));
        }
        if self.include_large_files {
            m.insert("includeLargeFiles".into(), Value::from(true));
        }
        m
    }

    fn block(&self) -> Value {
        json!({
            "id": self.id,
            "displayName": self.display_name,
            "source": self.source,
            "confidence": self.confidence,
            "detectedAt": iso_ms(util::now_ms()),
            "accepted": true,
            "modifiedByUser": self.preset_modified,
            "evidence": self.evidence,
        })
    }
}

/// Turn `--edit-add` / `--edit-remove` tokens into include-list edits. `.md` means `*.md`; a token
/// with a slash or a wildcard is taken as written.
fn apply_edit(include: &mut Vec<String>, adds: &[String], removes: &[String]) -> bool {
    let normalize = |t: &str| -> String {
        let t = t.trim();
        if t.starts_with('.') && !t.contains('/') {
            format!("*{t}")
        } else {
            t.to_string()
        }
    };
    let mut changed = false;
    for t in adds {
        if t.trim().is_empty() {
            continue;
        }
        let g = normalize(t);
        if !include.iter().any(|x| x == &g) {
            include.push(g);
            changed = true;
        }
    }
    for t in removes {
        if t.trim().is_empty() {
            continue;
        }
        let g = normalize(t);
        let before = include.len();
        include.retain(|x| x != &g);
        if include.len() != before {
            changed = true;
        }
    }
    include.sort();
    include.dedup();
    changed
}

enum Proceed {
    Yes,
    No,
    Edit,
}

/// The three-way prompt of SPEC §6.17: `Proceed? [Y/n/edit]`.
///
/// `yes` is also set by `--dry-run`: a dry run writes nothing, so refusing to answer a question
/// about a plan that will not be carried out helps nobody (round 295).
fn ask_proceed(yes: bool) -> Proceed {
    if yes {
        return Proceed::Yes;
    }
    if !stdin_is_tty() {
        println!("(no interactive terminal: re-run with --yes to confirm)");
        return Proceed::No;
    }
    print!("Proceed? [Y/n/edit] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Proceed::No;
    }
    match line.trim().to_lowercase().as_str() {
        "" | "y" | "yes" | "д" | "да" => Proceed::Yes,
        "e" | "edit" => Proceed::Edit,
        _ => Proceed::No,
    }
}

fn ask_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

/// The interactive `edit` branch. It edits the same include list that `--edit-add`/`--edit-remove`
/// edit, so the tested path and the typed path cannot drift apart.
fn interactive_edit(plan: &mut PresetPlan) {
    println!("Current include ({} patterns): {}", plan.include.len(), plan.include.join(" "));
    let adds = ask_line("Add extensions (space separated, empty to skip): ");
    let removes = ask_line("Remove patterns (space separated, empty to skip): ");
    let add_v: Vec<String> = adds.split_whitespace().map(|s| s.to_string()).collect();
    let rem_v: Vec<String> = removes.split_whitespace().map(|s| s.to_string()).collect();
    if apply_edit(&mut plan.include, &add_v, &rem_v) {
        plan.preset_modified = true;
    }
    let size = ask_line(&format!("Max file size in MB (currently {}): ", plan.max_kb / 1024));
    if let Ok(mb) = size.parse::<u64>() {
        if mb > 0 {
            plan.max_kb = mb * 1024;
            plan.preset_modified = true;
        }
    }
    println!("Now protecting: {}", if plan.include.is_empty() { "(nothing — that would protect no file)".to_string() } else { plan.include.join(" ") });
}

/// Detect, show, confirm — and return what the user agreed to.
fn build_preset_plan(root: &Path, arg: &str, a: &Args) -> Result<PresetPlan, String> {
    if let Some(pre) = profiles::system_root(root) {
        return Err(format!(
            "{} is a system location ({pre}): Project Life never protects one of those automatically",
            root.display()
        ));
    }
    let det = detect::detect_profile(root);
    print_detection(&det);

    let chosen: String = match arg {
        "auto" => {
            if det.suggest_custom {
                println!();
                println!("No profile reached {}% confidence.", detect::LOW_CONFIDENCE);
                if !stdin_is_tty() {
                    return Err(format!(
                        "no profile was convincing — re-run with an explicit preset ({}), or with --preset custom",
                        profiles::ids().join(", ")
                    ));
                }
                let answer = ask_line("Type a profile id, or press Enter for custom: ");
                if answer.is_empty() {
                    "custom".to_string()
                } else if profiles::by_id(&answer).is_some() || answer == "custom" {
                    answer
                } else {
                    return Err(format!("unknown profile `{answer}`"));
                }
            } else if det.ambiguous && stdin_is_tty() {
                println!();
                println!(
                    "Two profiles are close: {} ({}%) and {} ({}%).",
                    det.candidates[0].id, det.candidates[0].confidence, det.candidates[1].id,
                    det.candidates[1].confidence
                );
                let answer = ask_line(&format!("Choose [{}]: ", det.candidates[0].id));
                if profiles::by_id(&answer).is_some() || answer == "custom" {
                    answer
                } else {
                    det.best_id().unwrap_or_else(|| "custom".to_string())
                }
            } else {
                if det.ambiguous {
                    println!(
                        "note: {} and {} are within {} points; taking the higher one (--preset <name> overrides)",
                        det.candidates[0].id,
                        det.candidates[1].id,
                        detect::AMBIGUITY_GAP
                    );
                }
                det.best_id().unwrap_or_else(|| "custom".to_string())
            }
        }
        other => {
            if profiles::by_id(other).is_none() && other != "custom" {
                return Err(format!(
                    "unknown preset `{other}` — known presets: {}, custom",
                    profiles::ids().join(", ")
                ));
            }
            other.to_string()
        }
    };

    let (mut include, exclude, max_kb, media_left_out, display_name) = match profiles::by_id(&chosen) {
        Some(p) => {
            let (inc, left) = profiles::include_globs(p);
            (inc, profiles::exclude_globs(p), p.default_max_file_size_kb, left, p.display_name.to_string())
        }
        None => {
            let p = profiles::custom_profile();
            // `custom` starts from what the folder actually contains, not from nothing: every
            // extension that was seen becomes an include pattern (`*.abc`), and the user prunes the
            // list with --edit-remove or in the interactive edit. No policy filtering happens here:
            // choosing `custom` is the user saying "these files, exactly".
            let mut inc: Vec<String> = det
                .extension_counts
                .keys()
                .filter(|e| *e != "(none)")
                .map(|e| format!("*{e}"))
                .collect();
            inc.sort();
            (inc, profiles::exclude_globs(&p), p.default_max_file_size_kb, Vec::new(), p.display_name.to_string())
        }
    };

    let adds = a.list("edit-add");
    let removes = a.list("edit-remove");
    let mut preset_modified = apply_edit(&mut include, &adds, &removes);

    let include_secrets = a.has("include-secrets");
    let include_large_files = a.has("include-large-files");
    if include_secrets {
        println!();
        println!("WARNING: --include-secrets copies secrets into an archive that is not encrypted.");
    }

    let mut plan = PresetPlan {
        id: chosen.clone(),
        display_name,
        source: if arg == "auto" { "auto".to_string() } else { "manual".to_string() },
        confidence: det.confidence(),
        include,
        exclude,
        max_kb,
        media_left_out,
        preset_modified,
        include_secrets,
        include_large_files,
        evidence: json!({
            "extensions": det.extension_counts.iter().take(12).map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>(),
            "markers": det.markers,
            "folders": det.folder_hints,
            "entriesScanned": det.entries,
            "scanMs": det.elapsed_ms,
            "truncated": det.truncated,
        }),
    };

    loop {
        println!();
        println!("Preset: {}", plan.id);
        println!("Include ({} patterns): {}", plan.include.len(), plan.include.join(" "));
        println!("Exclude ({} patterns)", plan.exclude.len());
        println!("Max file size: {}", util::human_size(plan.max_kb * 1024));
        if !plan.media_left_out.is_empty() {
            println!("Not auto-included (media policy): {}", plan.media_left_out.join(" "));
        }
        if plan.preset_modified {
            println!("Edited by hand: this will be recorded as presetModifiedByUser.");
        }
        match ask_proceed(a.has("yes") || a.has("dry-run")) {
            Proceed::Yes => break,
            Proceed::No => {
                println!("cancelled");
                return Err("__cancelled__".to_string());
            }
            Proceed::Edit => {
                interactive_edit(&mut plan);
                preset_modified = plan.preset_modified;
            }
        }
    }
    let _ = preset_modified;
    Ok(plan)
}

fn cmd_add(rest: &[String], a: &Args) -> Result<i32, String> {
    let path = rest.first().ok_or_else(|| "usage: projectlife add <path>".to_string())?;
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("not a folder: {}", root.display()));
    }
    let arch = open_archive(a)?;
    let canon_root = root.canonicalize().unwrap_or(root.clone());
    if canon_root.starts_with(arch.root.canonicalize().unwrap_or(arch.root.clone()))
        || arch.root.canonicalize().unwrap_or(arch.root.clone()).starts_with(&canon_root)
    {
        return Err("the archive must not sit inside the project, and the project must not sit inside the archive".into());
    }
    for p in arch.load_projects()? {
        let other = PathBuf::from(&p.project_root);
        if other == root || canon_root.starts_with(&other) {
            if p.state() == "initializing" && other == root {
                // An interrupted initial copy resumes instead of refusing (FR-INI-3).
                println!("resuming an interrupted initial copy of {}", root.display());
                let mut project = p.clone();
                let opts = ScanOptions {
                    reason: "initial".into(),
                    deep: true,
                    verbose_filters: false,
                    dry_run: false,
                    with_initial_snapshot: true,
                    count_skipped: false,
                    scope: None,
                };
                let report = scan::scan_project(&arch, &mut project, &opts)?;
                println!(
                    "initial snapshot completed: {} new files, {} already present, {} skipped",
                    report.created + report.changed,
                    report.unchanged,
                    report.skipped
                );
                return Ok(EXIT_OK);
            }
            return Err(format!("a project is already being observed here: {} ({})", p.name, other.display()));
        }
        if other.starts_with(&canon_root) {
            return Err(format!("nested projects are not supported: {} is inside {}", other.display(), root.display()));
        }
    }
    let name = a.val("name").unwrap_or_else(|| {
        root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "project".into())
    });
    // Round 291 — universal mode. The preset is decided before a single byte is stored: detection is
    // metadata-only, and the include list it produces is what the initial snapshot will obey.
    let preset_plan = match a.val("preset") {
        Some(arg) => match build_preset_plan(&root, &arg, a) {
            Ok(p) => Some(p),
            Err(e) if e == "__cancelled__" => return Ok(EXIT_CANCELLED),
            Err(e) => return Err(e),
        },
        None => None,
    };
    let profile_flag = a.val("profile").unwrap_or_else(|| arch.config.str_of("profile", "source"));

    let preset_settings: Map<String, Value> = preset_plan.as_ref().map(|p| p.settings()).unwrap_or_default();
    let src_filter = if preset_plan.is_some() {
        FilterConfig::for_profile("preset", &preset_settings)
    } else {
        FilterConfig::for_profile("source", &Map::new())
    };
    let all_filter = FilterConfig::for_profile("all", &Map::new());
    let prefixes = vec![arch.root.clone()];
    let est_src = scan::estimate(&root, &src_filter, &prefixes);
    let est_all = scan::estimate(&root, &all_filter, &prefixes);
    let profile = if preset_plan.is_some() {
        "preset".to_string()
    } else if profile_flag == "ask" {
        println!("Profile source: {} files, {}", est_src.files, util::human_size(est_src.bytes));
        println!("Profile all:    {} files, {}", est_all.files, util::human_size(est_all.bytes));
        if !stdin_is_tty() {
            println!("(no interactive terminal: defaulting to source)");
            "source".to_string()
        } else {
            print!("Choose profile [source/all] (source): ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            match line.trim().to_lowercase().as_str() {
                "all" => "all".to_string(),
                _ => "source".to_string(),
            }
        }
    } else {
        profile_flag
    };
    let est = if profile == "all" { &est_all } else { &est_src };
    println!("Project:  {}", root.display());
    println!("Name:     {name}");
    println!("Profile:  {profile}");
    println!("Files:    {}", est.files);
    println!("Size:     {}", util::human_size(est.bytes));
    println!("Skipped:  {} entries", est.skipped_by_reason.values().sum::<usize>());
    for (reason, n) in &est.skipped_by_reason {
        println!("  - {reason}: {n}");
    }
    for (dir, reason, n) in &est.top_skipped_dirs {
        println!("  - {dir} ({reason}): {n} files");
    }
    let same_vol = doctor::same_volume(&root, &arch.root);
    if same_vol {
        println!("WARNING: the archive is on the same disk as the project: this will not protect against losing that disk or a mass deletion on it.");
    }
    // Round 295 — the desktop app shows what would be protected *before* anything exists, so the
    // estimate must be reachable without adding the folder. A dry run writes nothing at all: no
    // project directory, no journal, no blob, no config and no location file. It stops here, after
    // the same estimate and the same warnings the real run prints.
    if a.has("dry-run") {
        if a.json {
            let top: Vec<Value> = est
                .top_skipped_dirs
                .iter()
                .map(|(d, r, n)| json!({"path": d, "reason": r, "files": n}))
                .collect();
            let out = json!({
                "path": root.to_string_lossy(),
                "name": &name,
                "profile": &profile,
                "preset": preset_plan.as_ref().map(|p| p.block()),
                "files": est.files,
                "bytes": est.bytes,
                "skippedByReason": &est.skipped_by_reason,
                "topSkippedDirs": top,
                "sameVolume": same_vol,
                "created": false,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        } else {
            println!("dry run: nothing was created — no project, no journal and no copied bytes.");
        }
        return Ok(EXIT_OK);
    }
    let no_initial = a.has("no-initial");
    if no_initial {
        println!("WARNING: --no-initial: files get no previous version until their first change.");
    }
    if !no_initial && !confirm("Store the initial snapshot of matching files now?", a.has("yes")) {
        println!("cancelled");
        return Ok(EXIT_CANCELLED);
    }

    let id = util::uuid_v4();
    let dir = arch.projects_dir().join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    util::set_dir_owner_only(&dir);
    let now = iso_ms(util::now_ms());
    let mut meta = Map::new();
    meta.insert("schemaVersion".into(), Value::from(crate::SCHEMA_VERSION));
    meta.insert("projectId".into(), Value::from(id.clone()));
    meta.insert("name".into(), Value::from(name.clone()));
    meta.insert("projectRoot".into(), Value::from(root.to_string_lossy().to_string()));
    meta.insert("createdAt".into(), Value::from(now.clone()));
    meta.insert("historyStartsAt".into(), Value::from(now));
    meta.insert("profile".into(), Value::from(profile.clone()));
    match &preset_plan {
        Some(plan) => {
            meta.insert("settings".into(), Value::Object(plan.settings()));
            meta.insert("preset".into(), plan.block());
            // The same value at the top level, so a single grep finds it (SPEC §6.17).
            meta.insert("presetSource".into(), Value::from(plan.source.clone()));
            meta.insert("presetModifiedByUser".into(), Value::from(plan.preset_modified));
        }
        None => {
            meta.insert("settings".into(), Value::Object(Map::new()));
        }
    }
    meta.insert("state".into(), Value::from(if no_initial { "active" } else { "initializing" }));
    meta.insert("lastSeq".into(), Value::from(0));
    util::write_atomic(&dir.join("project.json"), serde_json::to_string_pretty(&Value::Object(meta)).unwrap_or_default().as_bytes())
        .map_err(|e| e.to_string())?;
    arch.log(&format!("add {name} ({}) profile={profile}", root.display()));

    let mut project = Project::from_dir(&dir)?;
    if let Some(plan) = &preset_plan {
        // SPEC §6.17 §8: the preset is written into the journal, not only into project.json, so a
        // reader of the journal alone can tell why this project tracks what it tracks.
        let mut ev = events::ev_new(0, util::now_ms(), "filters");
        events::put_str(&mut ev, "reason", "preset_applied");
        events::put_str(&mut ev, "preset", &plan.id);
        events::put_u64(&mut ev, "confidence", plan.confidence as u64);
        ev.insert("old".into(), json!({"include": [], "exclude": []}));
        ev.insert("new".into(), json!({"include": plan.include.clone(), "exclude": plan.exclude.clone()}));
        events::append(&project, &mut [ev]).map_err(|e| e.to_string())?;
    }
    if no_initial {
        println!("project added without an initial snapshot: {name} ({id})");
        return Ok(EXIT_OK);
    }
    let opts = ScanOptions {
        reason: "initial".into(),
        deep: true,
        verbose_filters: false,
        dry_run: false,
        with_initial_snapshot: true,
        count_skipped: false,
        scope: None,
    };
    let report = scan::scan_project(&arch, &mut project, &opts)?;
    println!(
        "initial snapshot: {} files stored ({} new blobs), {} skipped, {} of {} files already there",
        report.created + report.changed,
        report.blobs_new,
        report.skipped,
        report.unchanged,
        report.files_on_disk
    );
    if same_vol {
        arch.log("warning: archive and project share one volume");
    }
    Ok(EXIT_OK)
}

fn cmd_list(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let mut projects = arch.load_projects()?;
    if let Some(key) = a.val("sort") {
        sort_projects(&mut projects, &key)?;
    }
    if a.json {
        let arr: Vec<Value> = projects
            .iter()
            .map(|p| {
                json!({
                    "name": p.name, "id": p.id, "projectRoot": p.project_root, "state": p.state(),
                    "lastObservedAt": p.meta_str("lastObservedAt"),
                    "archiveBytes": p.size_on_disk(),
                    "profile": p.profile(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    if projects.is_empty() {
        println!("no projects yet. Add one: projectlife add <path>");
        return Ok(EXIT_OK);
    }
    println!("{:<24} {:<14} {:<20} {:>10}  path", "NAME", "STATE", "LAST OBSERVED", "SIZE");
    for p in &projects {
        println!(
            "{:<24} {:<14} {:<20} {:>10}  {}",
            p.name,
            p.state(),
            p.meta_str("lastObservedAt").map(|s| util::fmt_local(archive::parse_iso_ms(&s).unwrap_or(0))).unwrap_or_else(|| "-".into()),
            util::human_size(p.size_on_disk()),
            p.project_root
        );
    }
    Ok(EXIT_OK)
}

fn cmd_status(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let names: Vec<String> = if rest.is_empty() {
        arch.load_projects()?.iter().map(|p| p.name.clone()).collect()
    } else {
        vec![rest[0].clone()]
    };
    if names.is_empty() {
        println!("no projects yet");
        return Ok(EXIT_OK);
    }
    // One table for every project, with the one command that follows from each row.
    if a.has("compact") {
        return print_status_compact(&arch, rest.first().map(|s| s.as_str()));
    }
    let mut out_json: Vec<Value> = Vec::new();
    for name in &names {
        let p = arch.find(name)?;
        let journal = events::load_journal(&p.dir)?;
        let versions = journal.events.iter().filter(|e| events::event_type(e) == "put").count();
        let last = events::last_observed_at(&journal.events);
        let mut by_reason: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        let mut skipped_list: Vec<(String, String, String)> = Vec::new();
        for e in journal.events.iter().rev() {
            if events::event_type(e) == "skip" {
                let path = events::get_str(e, "path").unwrap_or_default();
                if skipped_list.iter().any(|(x, _, _)| x == &path) {
                    continue;
                }
                skipped_list.push((
                    path,
                    events::get_str(e, "reason").unwrap_or_default(),
                    events::get_str(e, "rule").unwrap_or_default(),
                ));
            }
        }
        for (_, r, _) in &skipped_list {
            *by_reason.entry(r.clone()).or_insert(0) += 1;
        }
        let interval = p.meta.get("observedIntervalMs").cloned().unwrap_or(Value::Null);
        let hb = arch.heartbeat_ms();
        let daemon_ok = hb.map(|t| util::now_ms() - t < 60_000).unwrap_or(false);
        if a.json {
            out_json.push(json!({
                "name": p.name, "id": p.id, "state": p.state(), "projectRoot": p.project_root,
                "profile": p.profile(), "versions": versions,
                "lastObservedAt": last.map(iso_ms),
                "archiveBytes": p.size_on_disk(),
                "skippedByReason": by_reason,
                "observedIntervalMs": interval,
                "daemonRunning": daemon_ok,
            }));
            continue;
        }
        println!("Project:      {} ({})", p.name, p.id);
        println!("Path:         {}", p.project_root);
        println!("State:        {}", p.state());
        println!("Profile:      {}", p.profile());
        println!("Versions:     {versions}");
        println!("Last observed: {}", last.map(util::fmt_local).unwrap_or_else(|| "never".into()));
        println!("Archive size: {}", util::human_size(p.size_on_disk()));
        match interval.get("median").and_then(|v| v.as_i64()) {
            Some(med) => println!(
                "Observed window: median {} ms, p95 {} ms ({} samples)",
                med,
                interval.get("p95").and_then(|v| v.as_i64()).unwrap_or(0),
                interval.get("samples").and_then(|v| v.as_u64()).unwrap_or(0)
            ),
            None => println!("Observed window: not measured yet"),
        }
        if !daemon_ok {
            println!("Daemon:       not running (or no cycle in the last 60 s)");
        }
        if !by_reason.is_empty() {
            let total: usize = by_reason.values().sum();
            println!("Skipped:      {total} paths");
            for (r, n) in &by_reason {
                println!("  - {r}: {n}");
            }
        }
        if a.has("skipped") {
            println!("Skipped paths:");
            for (path, reason, rule) in &skipped_list {
                println!("  {path}  [{reason}] {rule}");
            }
        }
        if a.has("risk") {
            println!("Risk:");
            let root = p.project_path();
            if doctor::same_volume(&root, &arch.root) {
                println!("  - archive and project share one volume");
            }
            if p.state() == "path_missing" {
                println!("  - the project path is missing");
            }
            if p.state() == "error" {
                println!("  - the project is in state error");
            }
            if let Some(gap) = p.meta_str("lastGapAt") {
                println!("  - last observation gap: {}", util::fmt_local(archive::parse_iso_ms(&gap).unwrap_or(0)));
            }
            let unreachable = journal
                .events
                .iter()
                .filter(|e| events::event_type(e) == "skip" && events::get_str(e, "reason").as_deref() == Some("unreadable"))
                .count();
            if unreachable > 0 {
                println!("  - unreadable files reported: {unreachable}");
            }
            let digest = newest_digest(&arch);
            match digest {
                Some(ts) => println!("  - last archive digest: {}", util::fmt_local(ts)),
                None => println!("  - no archive digest yet: run pl audit-archive --update"),
            }
            if let Some(ns) = skipped_list.iter().find(|(_, r, _)| r == "secret") {
                println!("  - secrets skipped (for example {})", ns.0);
            }
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&Value::Array(out_json)).unwrap_or_default());
    }
    Ok(EXIT_OK)
}

fn newest_digest(arch: &Archive) -> Option<i64> {
    let dir = arch.root.join("manifest");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).collect();
    files.sort();
    let f = files.last()?;
    let md = std::fs::metadata(f).ok()?;
    let modified = md.modified().ok()?;
    Some(util::ms_of(modified))
}

fn cmd_state_change(cmd: &str, rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| format!("usage: projectlife {cmd} <project>"))?;
    let mut p = arch.find(name)?;
    match cmd {
        "pause" => {
            let mut e = events::ev_new(0, util::now_ms(), "pause");
            events::put_str(&mut e, "reason", "user");
            let mut v = vec![e];
            events::append(&p, &mut v)?;
            p.set_meta("state", Value::from("paused"));
            p.save_meta()?;
            println!("paused: {}", p.name);
        }
        "resume" => {
            let mut e = events::ev_new(0, util::now_ms(), "resume");
            events::put_str(&mut e, "reason", "user");
            let mut v = vec![e];
            events::append(&p, &mut v)?;
            p.set_meta("state", Value::from("active"));
            p.save_meta()?;
            println!("resumed: {}", p.name);
        }
        _ => {
            p.set_meta("state", Value::from("removed"));
            p.save_meta()?;
            println!("removed from observation: {} (history kept; delete it with archive-delete)", p.name);
        }
    }
    Ok(EXIT_OK)
}

fn cmd_relink(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife relink <project> <new-path>".to_string())?;
    let new_path = rest.get(1).ok_or_else(|| "usage: projectlife relink <project> <new-path>".to_string())?;
    let np = PathBuf::from(new_path);
    if !np.is_dir() {
        return Err(format!("not a folder: {}", np.display()));
    }
    let mut p = arch.find(name)?;
    let old = p.project_root.clone();
    p.set_meta("projectRoot", Value::from(np.to_string_lossy().to_string()));
    p.set_meta("state", Value::from("active"));
    p.save_meta()?;
    let mut e = events::ev_new(0, util::now_ms(), "meta");
    events::put_str(&mut e, "reason", "relink");
    events::put_str(&mut e, "from", &old);
    events::put_str(&mut e, "to", &np.to_string_lossy());
    let mut v = vec![e];
    events::append(&p, &mut v)?;
    println!("relinked: {} -> {}", p.name, np.display());
    println!("Run `pl scan-once {}` to reconcile the new location.", p.name);
    Ok(EXIT_OK)
}

fn cmd_note(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife note <project> \"text\"".to_string())?;
    let text = rest.get(1).cloned().unwrap_or_default();
    let mut p = arch.find(name)?;
    p.set_meta("note", Value::from(text.clone()));
    p.save_meta()?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"project": p.name, "note": text})).unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("note saved for {}: {text}", p.name);
    Ok(EXIT_OK)
}

fn cmd_log(rest: &[String], a: &Args, pretty: bool) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife log <project>".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    let since = match a.val("since") {
        Some(s) => Some(util::parse_at(&s, util::now_ms())?),
        None => None,
    };
    let until = match a.val("until") {
        Some(s) => Some(util::parse_at(&s, util::now_ms())?),
        None => None,
    };
    let types: Vec<String> = a.val("type").map(|t| t.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default();
    let path_filter = a.val("path");
    let mut rows: Vec<Value> = Vec::new();
    for e in &journal.events {
        let ts = events::ts_of(e);
        if since.map(|s| ts < s).unwrap_or(false) || until.map(|u| ts > u).unwrap_or(false) {
            continue;
        }
        if !types.is_empty() && !types.iter().any(|t| t == events::event_type(e)) {
            continue;
        }
        if let Some(pf) = &path_filter {
            let norm = util::norm_rel(pf);
            let hit = [events::get_str(e, "path"), events::get_str(e, "to"), events::get_str(e, "from")]
                .into_iter()
                .flatten()
                .any(|x| x == norm || x.starts_with(&format!("{norm}/")));
            if !hit {
                continue;
            }
        }
        rows.push(Value::Object(e.clone()));
    }
    // `--grep` filters the events already selected; `--content` searches inside the stored bytes.
    if let Some(needle) = a.val("grep") {
        let g = needle.to_lowercase();
        rows.retain(|e| {
            let line = e.as_object().map(|m| serde_json::to_string(&Value::Object(m.clone())).unwrap_or_default());
            line.unwrap_or_default().to_lowercase().contains(&g)
        });
    }
    if let Some(needle) = a.val("content") {
        // Only stored versions are searched, and only small ones: this reads blobs, it does not
        // walk the project, and a version too big to be a text file is named, not read.
        let store = Store::new(&p.blobs_dir(), &p.tmp_dir());
        let limit_bytes = 1024 * 1024;
        let mut hits: Vec<Value> = Vec::new();
        let mut too_big = 0usize;
        for e in journal.events.iter().rev() {
            if events::event_type(e) != "put" {
                continue;
            }
            let (h, path) = match (events::get_str(e, "hash"), events::get_str(e, "path")) {
                (Some(h), Some(path)) => (h, path),
                _ => continue,
            };
            match store.size(&h) {
                Some(sz) if sz <= limit_bytes => match store.read_verified(&h) {
                    Ok(data) => {
                        if String::from_utf8_lossy(&data).contains(&needle) {
                            hits.push(json!({
                                "seq": events::seq_of(e), "at": iso_ms(events::ts_of(e)),
                                "path": path, "size": sz, "hash": h,
                            }));
                        }
                    }
                    Err(_) => {}
                },
                Some(_) => too_big += 1,
                None => {}
            }
            if hits.len() >= 200 {
                break;
            }
        }
        if a.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "project": p.name,
                    "needle": needle,
                    "matches": hits,
                    "tooBigToSearch": too_big,
                    "cap": 200,
                    "note": "only stored versions are searched, and only ones small enough to be text; a version that was never stored cannot match",
                }))
                .unwrap_or_default()
            );
            return Ok(EXIT_OK);
        }
        println!("{} versions contain \"{needle}\" ({} too big to search)", hits.len(), too_big);
        for h in hits.iter().rev() {
            println!(
                "{} {} {}",
                h.get("at").and_then(|v| v.as_str()).unwrap_or(""),
                h.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                h.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
            );
        }
        return Ok(EXIT_OK);
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    if rows.is_empty() {
        println!("no events match");
        return Ok(EXIT_OK);
    }
    for e in rows.iter().rev().take(if pretty { 200 } else { 200 }) {
        let m = e.as_object().unwrap();
        let ts = events::ts_of(m);
        let etype = events::event_type(m);
        let detail: String = match etype {
            "put" => format!(
                "{}  {} B  {}",
                events::get_str(m, "path").unwrap_or_default(),
                events::get_u64(m, "size").unwrap_or(0),
                events::get_str(m, "hash").unwrap_or_default().chars().take(12).collect::<String>()
            ),
            "delete" => events::get_str(m, "path").unwrap_or_default(),
            "move" => format!(
                "{} -> {}",
                events::get_str(m, "from").unwrap_or_default(),
                events::get_str(m, "to").unwrap_or_default()
            ),
            "skip" => format!(
                "{}  [{}] {}",
                events::get_str(m, "path").unwrap_or_default(),
                events::get_str(m, "reason").unwrap_or_default(),
                events::get_str(m, "rule").unwrap_or_default()
            ),
            "mass" => format!(
                "{}: {} files ({} deleted, {} changed, {} created), last good seq {}",
                events::get_str(m, "kind").unwrap_or_default(),
                events::get_u64(m, "files").unwrap_or(0),
                events::get_u64(m, "deleted").unwrap_or(0),
                events::get_u64(m, "changed").unwrap_or(0),
                events::get_u64(m, "created").unwrap_or(0),
                events::get_u64(m, "lastGoodSeq").unwrap_or(0)
            ),
            "gap" => format!(
                "{} .. {} ({})",
                util::fmt_local(events::get_i64(m, "from").unwrap_or(0)),
                util::fmt_local(events::get_i64(m, "to").unwrap_or(0)),
                events::get_str(m, "reason").unwrap_or_default()
            ),
            "snapshot" => format!(
                "{} ({} files)",
                events::get_str(m, "reason").unwrap_or_default(),
                events::get_u64(m, "files").unwrap_or(0)
            ),
            "mark" => events::get_str(m, "label").unwrap_or_default(),
            "meta" => events::get_str(m, "reason").unwrap_or_default(),
            _ => events::get_str(m, "reason").unwrap_or_default(),
        };
        println!("{:>6}  {}  {:<9} {}", events::seq_of(m), util::fmt_local(ts), etype, detail);
    }
    Ok(EXIT_OK)
}

fn cmd_tree(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife tree <project> --at T".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    let (ts, label) = parse_moment(a, &p, &journal.events)?;
    let hstart = p.history_starts_at();
    if hstart > 0 && ts < hstart {
        let (m, h) = util::fmt_moment_pair(ts, hstart);
        return Err(format!(
            "moment {} is earlier than the start of the available history {}.\nAvailable range: {} … {}\n(use `pl log {}` to see the boundaries)",
            m,
            h,
            util::fmt_local(hstart),
            util::fmt_local(events::last_observed_at(&journal.events).unwrap_or(util::now_ms())),
            p.name
        ));
    }
    let state = events::state_at(&journal.events, ts, None);
    if a.json {
        let items: Vec<Value> = state
            .iter()
            .map(|(k, v)| json!({"path": k, "size": v.size, "hash": v.hash, "kind": v.kind, "mode": v.mode}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    println!("{} at {} ({})", p.name, util::fmt_local(ts), label);
    for (k, v) in &state {
        let tag = if v.kind == "symlink" { "->" } else { " " };
        println!("{:>10} {} {}{}", util::human_size(v.size), v.hash.chars().take(12).collect::<String>(), tag, k);
    }
    println!("{} files", state.len());
    Ok(EXIT_OK)
}

fn cmd_diff(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife diff <project> --at T [--to T2|--current]".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    let (from_ts, from_label) = parse_moment(a, &p, &journal.events)?;
    let (to_ts, to_label) = if a.has("current") {
        (util::now_ms(), "current disk".to_string())
    } else if let Some(t2) = a.val("to") {
        (util::parse_at(&t2, util::now_ms())?, t2)
    } else {
        (
            events::last_observed_at(&journal.events).unwrap_or(util::now_ms()),
            "last observed".to_string(),
        )
    };
    let a_state = events::state_at(&journal.events, from_ts, None);
    let b_state = if a.has("current") {
        // current disk state, read through the filters
        let filter = FilterConfig::for_profile(&p.profile(), &p.settings());
        let (own, git) = scan::open_ignore_rules(&p.project_path());
        let prefixes = vec![arch.root.clone()];
        let (cur, _sk, _er) = scan::walk(&p.project_path(), &filter, &prefixes, &own, &git, false);
        let mut m = std::collections::BTreeMap::new();
        for (rel, cf) in cur {
            m.insert(
                rel,
                events::FileSt {
                    hash: String::new(),
                    size: cf.size,
                    mode: cf.mode,
                    kind: "file".into(),
                    target: None,
                    seq: 0,
                    ts: cf.mtime,
                },
            );
        }
        m
    } else {
        events::state_at(&journal.events, to_ts, None)
    };
    let path_filter = a.val("path").map(|x| util::norm_rel(&x));
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (k, v) in &b_state {
        if let Some(pf) = &path_filter {
            if !(k == pf || k.starts_with(&format!("{pf}/"))) {
                continue;
            }
        }
        match a_state.get(k) {
            None => added.push(k.clone()),
            Some(old) => {
                if old.hash != v.hash && !v.hash.is_empty() && !old.hash.is_empty() {
                    changed.push(k.clone());
                } else if old.size != v.size {
                    changed.push(k.clone());
                }
            }
        }
    }
    for k in a_state.keys() {
        if let Some(pf) = &path_filter {
            if !(k == pf || k.starts_with(&format!("{pf}/"))) {
                continue;
            }
        }
        if !b_state.contains_key(k) {
            removed.push(k.clone());
        }
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "from": {"ts": from_ts, "label": from_label},
                "to": {"ts": to_ts, "label": to_label},
                "added": added, "removed": removed, "changed": changed,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("{}: {} -> {}", p.name, util::fmt_local(from_ts), util::fmt_local(to_ts));
    if a.has("stat") || !a.has("name-status") && !a.has("files-only") && !a.has("content") {
        println!("  +{} added, -{} removed, ~{} changed", added.len(), removed.len(), changed.len());
        if !a.has("content") {
            return Ok(EXIT_OK);
        }
    }
    if a.has("name-status") || a.has("files-only") {
        for k in &added {
            println!("A {k}");
        }
        for k in &removed {
            println!("D {k}");
        }
        for k in &changed {
            println!("M {k}");
        }
        return Ok(EXIT_OK);
    }
    if a.has("content") {
        let store = Store::new(&p.blobs_dir(), &p.tmp_dir());
        for k in &changed {
            println!("--- {k}");
            let a_text = a_state.get(k).and_then(|s| store.read_verified(&s.hash).ok()).map(|b| String::from_utf8_lossy(&b).to_string());
            let b_text = b_state.get(k).and_then(|s| store.read_verified(&s.hash).ok()).map(|b| String::from_utf8_lossy(&b).to_string());
            match (a_text, b_text) {
                (Some(x), Some(y)) => {
                    for line in x.lines() {
                        if !y.lines().any(|l| l == line) {
                            println!("- {line}");
                        }
                    }
                    for line in y.lines() {
                        if !x.lines().any(|l| l == line) {
                            println!("+ {line}");
                        }
                    }
                }
                _ => println!("  (binary or unavailable content)"),
            }
        }
    }
    Ok(EXIT_OK)
}

fn cmd_restore(rest: &[String], a: &Args, export_file: bool) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife restore <project> --at T".to_string())?;
    let mut project = arch.find(name)?;
    heal_prune(&arch, &mut project);
    let journal = events::load_journal(&project.dir)?;
    let (ts, label) = parse_moment(a, &project, &journal.events)?;
    let mut paths = a.list("path");
    if export_file && paths.is_empty() {
        paths = rest.iter().skip(1).cloned().collect();
    }
    let into_project = a.has("into-project");
    let clean = a.has("clean");
    let missing = a.has("missing");
    if clean && !into_project {
        return Err("--clean is only allowed together with --into-project".into());
    }
    if clean && missing {
        return Err("--missing only creates and --clean deletes: choose one".into());
    }
    let opts = RestoreOptions {
        at: ts,
        at_label: label,
        paths,
        to: a.val("to").map(PathBuf::from),
        into_project,
        clean,
        preview: a.has("preview"),
        missing,
    };
    let plan = restore::plan(&arch, &project, &opts)?;
    if a.json && opts.preview {
        // A preview that is asked for as data must write nothing and print no prose: the window
        // shows the plan before the person agrees to it, and reads the same numbers here.
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project.name,
                "moment": plan.moment,
                "momentIso": iso_ms(plan.moment),
                "momentLocal": util::fmt_local(plan.moment),
                "atLabel": plan.at_label,
                "target": plan.target.to_string_lossy(),
                "intoProject": into_project,
                "clean": clean,
                "missing": plan.missing,
                "create": plan.create,
                "overwrite": plan.overwrite,
                "total": plan.total(),
                "present": plan.present,
                "deleteExtra": plan.delete_extra,
                "bytes": plan.bytes,
                "missingBlobs": plan.missing_blobs,
                "symlinks": plan.symlinks,
                "warnings": plan.warnings,
                "files": plan.files.iter().map(|(p, st)| json!({
                    "path": p, "size": st.size, "kind": st.kind,
                })).collect::<Vec<Value>>(),
                "extra": plan.extra,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    if !a.json {
        print_plan(&plan, &project, into_project);
    }
    if opts.preview {
        return Ok(EXIT_OK);
    }
    if into_project {
        if !confirm_name(
            "Restore INTO the project folder (current files will be overwritten)?",
            &project.name,
            a.has("yes"),
        ) {
            println!("cancelled");
            return Ok(EXIT_CANCELLED);
        }
        if clean && plan.delete_extra > 0 {
            let big = plan.delete_extra > 50 || (plan.delete_extra as f64) > 0.1 * (plan.total().max(1) as f64);
            if big
                && !confirm_name(
                    &format!("--clean will DELETE {} files that did not exist at that moment.", plan.delete_extra),
                    &project.name,
                    a.has("yes"),
                )
            {
                println!("cancelled");
                return Ok(EXIT_CANCELLED);
            }
        }
        // force an out-of-band observation first, so the current state also reaches the archive
        let mut p2 = project.clone();
        let sopts = ScanOptions {
            reason: "pre_restore".into(),
            deep: false,
            verbose_filters: false,
            dry_run: false,
            with_initial_snapshot: false,
            count_skipped: false,
            scope: None,
        };
        match scan::scan_project(&arch, &mut p2, &sopts) {
            Ok(rep) => {
                if !a.json {
                    println!("pre-restore observation: {} files read, {} new versions", rep.files_on_disk, rep.created + rep.changed)
                }
            }
            Err(e) => {
                if !a.json {
                    println!("pre-restore observation failed ({e}); continuing with the existing history")
                }
            }
        }
    } else if plan.target.exists() && !missing {
        return Err(format!("target already exists: {} (choose another --to)", plan.target.display()));
    }
    // A plain restore into a separate folder is non-destructive: it proceeds without a prompt.
    // Writing into the project or deleting extras is destructive and is confirmed above.
    if into_project {
        let question = if missing {
            "Put the missing files back into the project folder? Nothing that exists will be overwritten or deleted."
        } else {
            "Proceed with the restore?"
        };
        if !confirm(question, a.has("yes")) {
            println!("cancelled");
            return Ok(EXIT_CANCELLED);
        }
    }
    // A thinned history must say so where it matters: at the moment someone asks for a state.
    if let (Ok(Some(t)), false) = (retention::thinned_at(&project), a.json) {
        println!(
            "note: this history was thinned by a retention policy ({}). The state above is the newest\n\
             retained moment at or before the moment you asked for; the versions between are gone.",
            util::fmt_local(t)
        );
    }
    let report = restore::execute(&project, &plan, &opts)?;
    if a.json {
        // One object, no prose: the window runs a repair and then checks the result itself, and it
        // must not have to parse a sentence to learn how many files were written.
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project.name,
                "moment": plan.moment,
                "momentIso": iso_ms(plan.moment),
                "target": plan.target.to_string_lossy(),
                "intoProject": into_project,
                "missing": opts.missing,
                "restored": report.restored,
                "restoredPaths": report.restored_paths,
                "failed": report.failed,
                "deleted": report.deleted,
                "skippedPresent": report.skipped_present,
                "skippedSymlinks": report.skipped_symlinks,
                "bytes": report.bytes,
                "missingBlobs": report.missing,
                "warnings": report.warnings,
            }))
            .unwrap_or_default()
        );
        return Ok(if report.failed > 0 { EXIT_PARTIAL } else { EXIT_OK });
    }
    println!(
        "restored {} files ({}) into {}",
        report.restored,
        util::human_size(report.bytes),
        plan.target.display()
    );
    if report.deleted > 0 {
        println!("deleted {} extra files (--clean)", report.deleted);
    }
    if report.skipped_present > 0 {
        println!(
            "{} file(s) appeared on disk while the repair was running and were left as they are",
            report.skipped_present
        );
    }
    if report.skipped_symlinks > 0 {
        println!("{} symlinks skipped (target outside the project)", report.skipped_symlinks);
    }
    if !report.warnings.is_empty() {
        println!("warnings:");
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
    println!("NOTE: empty directories are not restored (files inside them are).");
    ops::record(
        &arch,
        "restore",
        &project.name,
        json!({
            "at": iso_ms(opts.at), "atLabel": opts.at_label, "target": plan.target.to_string_lossy(),
            "intoProject": into_project, "missing": opts.missing, "restored": report.restored, "failed": report.failed,
            "deleted": report.deleted, "skippedPresent": report.skipped_present,
        }),
    );
    if report.failed > 0 {
        println!("{} files could not be restored (missing or corrupted blobs)", report.failed);
        return Ok(EXIT_PARTIAL);
    }
    Ok(EXIT_OK)
}

fn cmd_mark(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife mark <project> \"label\"".to_string())?;
    let label = rest.get(1).cloned().ok_or_else(|| "usage: projectlife mark <project> \"label\"".to_string())?;
    let p = arch.find(name)?;
    let mut e = events::ev_new(0, util::now_ms(), "mark");
    events::put_str(&mut e, "label", &label);
    let mut v = vec![e];
    events::append(&p, &mut v)?;
    let at = util::now_ms();
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name, "label": label, "at": at, "atIso": iso_ms(at), "atLocal": util::fmt_local(at)
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("mark \"{label}\" saved for {} at {}", p.name, util::fmt_local(at));
    Ok(EXIT_OK)
}

fn cmd_last_good(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife last-good <project>".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    let found = restore::last_good(&journal.events, &p);
    if a.json {
        let v = match &found {
            Some((ts, why)) => json!({
                "project": p.name, "found": true, "at": ts, "atIso": iso_ms(*ts),
                "atLocal": util::fmt_local(*ts), "why": why,
                "restoreTo": format!("../{}-recovered", p.name),
            }),
            None => json!({
                "project": p.name, "found": false, "at": Value::Null,
                "why": "no mass event has been recorded, so there is no last-good point to compute",
            }),
        };
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    match found {
        Some((ts, why)) => {
            println!("last good state: {} ({why})", util::fmt_local(ts));
            println!("restore: pl restore {} --last-good --to ../{}-recovered", p.name, p.name);
        }
        None => {
            println!("no mass events in {}: there is no \"last good\" point to compute.", p.name);
            println!("use --at with any moment: pl restore {} --at \"10m ago\" --to <dir>", p.name);
        }
    }
    Ok(EXIT_OK)
}

fn cmd_panic(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife panic <project>".to_string())?;
    let project = arch.find(name)?;
    let journal = events::load_journal(&project.dir)?;
    let last = events::last_observed_at(&journal.events);
    let mut options: Vec<(i64, String)> = Vec::new();
    if let Some((ts, why)) = restore::last_good(&journal.events, &project) {
        options.push((ts, format!("last good state — {why}")));
    }
    let now = util::now_ms();
    options.push((now - 300_000, "5 minutes ago".into()));
    options.push((now - 600_000, "10 minutes ago".into()));
    if let Some(m) = journal
        .events
        .iter()
        .filter(|e| events::event_type(e) == "mark")
        .max_by_key(|e| events::ts_of(e))
    {
        options.push((events::ts_of(m), format!("last mark: {}", events::get_str(m, "label").unwrap_or_default())));
    }
    if let Some(ts) = last {
        options.push((ts, "last observed moment".into()));
    }
    println!("{}: {} candidate restore points", project.name, options.len());
    for (i, (ts, why)) in options.iter().enumerate() {
        println!("  {}. {} — {why}", i + 1, util::fmt_local(*ts));
    }
    let chosen = if let Some(at) = a.val("at") {
        (util::parse_at(&at, now)?, at)
    } else if a.has("yes") {
        options[0].clone()
    } else if stdin_is_tty() {
        print!("Choose 1-{} (1): ", options.len());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let idx: usize = line.trim().parse().unwrap_or(1);
        options.get(idx.saturating_sub(1)).cloned().unwrap_or_else(|| options[0].clone())
    } else {
        println!("(no interactive terminal: using option 1, pass --at to choose another)");
        options[0].clone()
    };
    println!("restoring {} into a separate folder", util::fmt_local(chosen.0));
    let opts = RestoreOptions {
        at: chosen.0,
        at_label: chosen.1.clone(),
        paths: Vec::new(),
        to: a.val("to").map(PathBuf::from),
        into_project: false,
        clean: false,
        preview: a.has("preview"),
        missing: false,
    };
    let plan = restore::plan(&arch, &project, &opts)?;
    print_plan(&plan, &project, false);
    if a.has("preview") {
        return Ok(EXIT_OK);
    }
    if plan.target.exists() {
        return Err(format!("target already exists: {}", plan.target.display()));
    }
    let report = restore::execute(&project, &plan, &opts)?;
    println!("restored {} files into {}", report.restored, plan.target.display());
    println!("The project folder was NOT touched. Compare, then replace it yourself if the result is right.");
    ops::record(
        &arch,
        "panic",
        &project.name,
        json!({"at": iso_ms(chosen.0), "atLabel": chosen.1, "target": plan.target.to_string_lossy(), "restored": report.restored}),
    );
    if report.failed > 0 {
        return Ok(EXIT_PARTIAL);
    }
    Ok(EXIT_OK)
}

fn cmd_why(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife why <project> <path>".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    // One function, two readers: the person gets the same sentences as before, a program gets the
    // same facts as JSON. Nothing is computed twice and nothing is printed in json mode.
    let say = |s: String| {
        if !a.json {
            println!("{s}");
        }
    };
    let emit = |v: &Value| {
        if a.json {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        }
    };
    if a.has("at") && rest.len() < 2 {
        let (ts, label) = parse_moment(a, &p, &journal.events)?;
        let hstart = p.history_starts_at();
        let mut rep = json!({
            "project": p.name,
            "mode": "moment",
            "moment": ts,
            "momentIso": iso_ms(ts),
            "momentLocal": util::fmt_local(ts),
            "label": label,
            "historyStartsAt": if hstart > 0 { json!(hstart) } else { Value::Null },
            "historyStartsAtLocal": if hstart > 0 { json!(util::fmt_local(hstart)) } else { Value::Null },
            "available": true,
            "reason": Value::Null,
            "filesKnown": Value::Null,
            "nearestObservedBefore": Value::Null,
            "nearestObservedBeforeLocal": Value::Null,
            "insideGap": Value::Null,
        });
        say(format!("Moment: {} ({label})", util::fmt_local(ts)));
        if hstart > 0 && ts < hstart {
            let created = p.meta_str("createdAt").and_then(|s| archive::parse_iso_ms(&s)).unwrap_or(0);
            say(format!("Not available: it is earlier than historyStartsAt ({}).", util::fmt_local(hstart)));
            let reason = if (hstart - created).abs() < 2000 {
                say("Reason: the project was added at that moment — nothing earlier was ever observed.".into());
                "the project was added at that moment — nothing earlier was ever observed"
            } else {
                say("Reason: pruning removed earlier versions; only the state at that boundary survives.".into());
                "pruning removed earlier versions; only the state at that boundary survives"
            };
            rep["available"] = json!(false);
            rep["reason"] = json!(reason);
            emit(&rep);
            return Ok(EXIT_OK);
        }
        for (from, to, reason) in events::gaps(&journal.events) {
            if ts >= from && ts <= to {
                say(format!("Inside an observation gap: {} .. {} ({reason}).", util::fmt_local(from), util::fmt_local(to)));
                say("Reason: the program was not running/observing then; only the nearest previous state exists.".into());
                rep["reason"] = json!("inside an observation gap");
                rep["insideGap"] = json!({"from": from, "fromLocal": util::fmt_local(from), "to": to, "toLocal": util::fmt_local(to), "why": reason});
                emit(&rep);
                return Ok(EXIT_OK);
            }
        }
        let state = events::state_at(&journal.events, ts, None);
        let prior: Vec<Ev> = journal.events.iter().filter(|e| events::ts_of(e) <= ts).cloned().collect();
        let nearest = events::last_observed_at(&prior).unwrap_or(0);
        say(format!("Files known at that moment: {}", state.len()));
        say(format!("Nearest observed moment before it: {}", util::fmt_local(nearest)));
        rep["filesKnown"] = json!(state.len());
        rep["nearestObservedBefore"] = json!(nearest);
        rep["nearestObservedBeforeLocal"] = json!(util::fmt_local(nearest));
        emit(&rep);
        return Ok(EXIT_OK);
    }
    let path = rest.get(1).ok_or_else(|| "usage: projectlife why <project> <path>".to_string())?;
    let rel = util::norm_rel(path);
    let versions: Vec<&Ev> = journal
        .events
        .iter()
        .filter(|e| {
            events::event_type(e) == "put"
                && (events::get_str(e, "path").as_deref() == Some(rel.as_str())
                    || events::get_str(e, "path").map(|x| x.starts_with(&format!("{rel}/"))).unwrap_or(false))
        })
        .collect();
    let mut rep = json!({
        "project": p.name,
        "mode": "path",
        "path": rel,
        "tracked": !versions.is_empty(),
        "versionCount": versions.len(),
        "versions": versions.iter().map(|e| json!({
            "seq": events::seq_of(e),
            "at": events::ts_of(e),
            "atIso": iso_ms(events::ts_of(e)),
            "atLocal": util::fmt_local(events::ts_of(e)),
            "size": events::get_u64(e, "size").unwrap_or(0),
            "hash": events::get_str(e, "hash").unwrap_or_default(),
        })).collect::<Vec<Value>>(),
        "lastVersion": Value::Null,
        "decision": Value::Null,
        "parentDecision": Value::Null,
        "lastSkip": Value::Null,
    });
    say(format!("Path:    {rel}"));
    if versions.is_empty() {
        say("Tracked: no versions in the archive".into());
    } else {
        say(format!("Tracked: yes, {} version(s)", versions.len()));
        if let Some(last) = versions.last() {
            say(format!(
                "Last version: {} ({} B, {})",
                util::fmt_local(events::ts_of(last)),
                events::get_u64(last, "size").unwrap_or(0),
                events::get_str(last, "hash").unwrap_or_default()
            ));
            rep["lastVersion"] = json!({
                "seq": events::seq_of(last),
                "at": events::ts_of(last),
                "atIso": iso_ms(events::ts_of(last)),
                "atLocal": util::fmt_local(events::ts_of(last)),
                "size": events::get_u64(last, "size").unwrap_or(0),
                "hash": events::get_str(last, "hash").unwrap_or_default(),
            });
        }
    }
    let filter = FilterConfig::for_profile(&p.profile(), &p.settings());
    let (own, git) = scan::open_ignore_rules(&p.project_path());
    let decision = filter.decide_file(&rel, 0, &own, &git);
    let reason = decision.reason().unwrap_or("?").to_string();
    let rule = decision.rule.clone().unwrap_or_default();
    rep["decision"] = json!({"track": decision.track, "reason": reason, "rule": rule, "profile": format!("{:?}", p.profile())});
    if decision.track {
        say(format!("Decision: tracked (matches the {:?} profile)", p.profile()));
    } else {
        say(format!("Decision: NOT tracked — reason {reason} (rule: {rule})"));
        say(format!(
            "Fix: add the path to \"include\" in project.json, or change the profile, then run `pl apply-filters {}`",
            p.name
        ));
    }
    if let Some(dir) = rel.rsplit_once('/').map(|(d, _)| d) {
        let dd = filter.decide_dir(dir, &[], &own, &git);
        rep["parentDecision"] = json!({
            "dir": dir, "track": dd.track,
            "reason": dd.reason().unwrap_or("?"), "rule": dd.rule.clone().unwrap_or_default(),
        });
        if !dd.track {
            let reason = dd.reason().unwrap_or("?").to_string();
            let rule = dd.rule.clone().unwrap_or_default();
            say(format!("Parent folder is skipped: reason {reason} (rule {rule})"));
        }
    }
    if let Some(skip) = journal.events.iter().rev().find(|e| {
        events::event_type(e) == "skip" && events::get_str(e, "path").as_deref() == Some(rel.as_str())
    }) {
        say(format!(
            "Last skip event: {} [{}] {}",
            util::fmt_local(events::ts_of(skip)),
            events::get_str(skip, "reason").unwrap_or_default(),
            events::get_str(skip, "rule").unwrap_or_default()
        ));
        rep["lastSkip"] = json!({
            "seq": events::seq_of(skip), "at": events::ts_of(skip),
            "atIso": iso_ms(events::ts_of(skip)), "atLocal": util::fmt_local(events::ts_of(skip)),
            "reason": events::get_str(skip, "reason").unwrap_or_default(),
            "rule": events::get_str(skip, "rule").unwrap_or_default(),
        });
    }
    emit(&rep);
    Ok(EXIT_OK)
}

fn cmd_apply_filters(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife apply-filters <project>".to_string())?;
    let mut p = arch.find(name)?;
    let before = scan::StateCache::load(&p, &events::load_journal(&p.dir)?.events);
    let json_mode = a.json;
    let opts = ScanOptions {
        reason: "filters_changed".into(),
        deep: false,
        verbose_filters: true,
        dry_run: false,
        with_initial_snapshot: false,
        count_skipped: false,
        scope: None,
    };
    let report = scan::scan_project(&arch, &mut p, &opts)?;
    let now_tracked: std::collections::BTreeSet<String> = report
        .skipped_paths
        .iter()
        .map(|(p, _, _)| p.clone())
        .collect();
    let started: Vec<&String> = now_tracked.iter().collect();
    let stopped: Vec<String> = before.files.keys().filter(|k| now_tracked.contains(*k)).cloned().collect();
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name,
                "filesOnDisk": report.files_on_disk,
                "newVersions": report.created + report.changed,
                "bytesRead": report.bytes_read,
                "skippedNow": started.iter().map(|s| s.to_string()).collect::<Vec<String>>(),
                "noLongerTracked": stopped.clone(),
                "skippedByReason": report.skipped_by_reason,
                "note": "earlier versions of a path that is no longer tracked are kept: the filter decides what is observed, never what is deleted",
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("filters applied to {}", p.name);
    println!("  tracked now: {} files", report.files_on_disk);
    println!("  new versions: {} ({})", report.created + report.changed, util::human_size(report.bytes_read));
    if !stopped.is_empty() {
        println!("  no longer tracked (earlier versions are kept): {}", stopped.len());
        for s in stopped.iter().take(20) {
            println!("    - {s}");
        }
    }
    if !started.is_empty() {
        println!("  skipped now: {}", started.len());
        for (reason, n) in &report.skipped_by_reason {
            println!("    - {reason}: {n}");
        }
    }
    Ok(EXIT_OK)
}

fn cmd_prune(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife prune <project> --before T | --policy <policy>".to_string())?;
    // `--policy` and `--before` keep different promises: the first keeps one moment per bucket, the
    // second cuts the history at a single date. They are not combined, and neither runs by itself.
    if let Some(spec) = a.val("policy") {
        if a.has("before") {
            return Err("give either --before or --policy, not both: they prune differently".into());
        }
        return apply_policy(&arch, name, &spec, a, a.has("dry-run"));
    }
    let before_spec = a.val("before").ok_or_else(|| "prune requires --before <date> or --policy <policy>".to_string())?;
    let before = util::parse_at(&before_spec, util::now_ms())?;
    let mut p = arch.find(name)?;
    heal_prune(&arch, &mut p);
    let plan = lifecycle::prune_plan(&p, before)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name,
                "mode": "before",
                "before": before,
                "beforeIso": iso_ms(before),
                "beforeLocal": util::fmt_local(before),
                "spec": before_spec,
                "versionsBefore": plan.versions_before,
                "versionsAfter": plan.versions_after,
                "blobsBefore": plan.blobs_before,
                "blobsAfter": plan.blobs_after,
                "bytesBefore": plan.bytes_before,
                "anchors": plan.anchors,
                "eventsKept": plan.keep_events,
                "newHistoryStartsAt": before,
                "dryRun": a.has("dry-run"),
                "applied": false,
            }))
            .unwrap_or_default()
        );
        if a.has("dry-run") {
            return Ok(EXIT_OK);
        }
        if !confirm_name("Prune this history?", &p.name, a.has("yes")) {
            return Ok(EXIT_CANCELLED);
        }
        let done = lifecycle::prune(&arch, &mut p, before)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name, "mode": "before", "applied": true, "dryRun": false,
                "versionsBefore": done.versions_before, "versionsAfter": done.versions_after,
                "blobsBefore": done.blobs_before, "blobsAfter": done.blobs_after,
                "newHistoryStartsAt": before, "newHistoryStartsAtLocal": util::fmt_local(before),
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("Project: {}", p.name);
    println!("Before:  {} ({before_spec})", util::fmt_local(before));
    println!("Versions: {} -> {}", plan.versions_before, plan.versions_after);
    println!("Blobs:    {} -> {}", plan.blobs_before, plan.blobs_after);
    println!("Size now: {}", util::human_size(plan.bytes_before));
    println!("Anchors kept: {} files (the state at that moment stays restorable)", plan.anchors);
    println!("Events kept after the boundary: {}", plan.keep_events);
    println!("New historyStartsAt: {}", util::fmt_local(before));
    if a.has("dry-run") {
        println!("dry run: nothing was changed");
        return Ok(EXIT_OK);
    }
    if !confirm_name("Prune this history?", &p.name, a.has("yes")) {
        println!("cancelled");
        return Ok(EXIT_CANCELLED);
    }
    let done = lifecycle::prune(&arch, &mut p, before)?;
    println!("pruned: versions {} -> {}, blobs {} -> {}", done.versions_before, done.versions_after, done.blobs_before, done.blobs_after);
    println!("Restoring a moment earlier than {} will now be refused with an explanation.", util::fmt_local(before));
    Ok(EXIT_OK)
}

fn cmd_export(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife export <project> --out <dir>".to_string())?;
    let out = a.val("out").ok_or_else(|| "export requires --out <dir>".to_string())?;
    let mut p = arch.find(name)?;
    heal_prune(&arch, &mut p);
    let from = match a.val("from") {
        Some(s) => Some(util::parse_at(&s, util::now_ms())?),
        None => None,
    };
    let to = match a.val("to") {
        Some(s) => Some(util::parse_at(&s, util::now_ms())?),
        None => None,
    };
    let out_path = PathBuf::from(&out);
    let rep = lifecycle::export(&arch, &p, from, to, &out_path)?;
    println!("export written: {}", rep.out.display());
    println!("  events: {}", rep.files);
    println!("  blobs:  {} ({})", rep.blobs, util::human_size(rep.bytes));
    println!("  manifest: {} (verify with: cd {} && sha256sum -c MANIFEST.sha256)", rep.manifest.display(), rep.out.display());
    if !rep.verified {
        println!("EXPORT VERIFICATION FAILED: see EXPORT_FAILED.txt. Do not delete anything on the basis of this export.");
        return Ok(EXIT_PARTIAL);
    }
    println!("  verification: every blob re-read and matched against its name — OK");
    ops::record(
        &arch,
        "export",
        &p.name,
        json!({"out": rep.out.to_string_lossy(), "files": rep.files, "blobs": rep.blobs, "verified": rep.verified}),
    );
    if a.has("pack") {
        let tar = out_path.with_extension("tar");
        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(out_path.parent().unwrap_or(Path::new(".")))
            .arg(out_path.file_name().unwrap_or_default())
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            println!("  packed (uncompressed): {}", tar.display());
        } else {
            println!("  packing failed: create the tar yourself from {}", out_path.display());
        }
    }
    Ok(EXIT_OK)
}

fn cmd_export_and_prune(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife export-and-prune <project> --before T --out <dir>".to_string())?;
    let out = a.val("out").ok_or_else(|| "required: --out <dir>".to_string())?;
    let before = util::parse_at(&a.val("before").ok_or_else(|| "required: --before <date>".to_string())?, util::now_ms())?;
    let mut p = arch.find(name)?;
    heal_prune(&arch, &mut p);
    println!("step 1/4: export up to {}", util::fmt_local(before));
    let rep = lifecycle::export(&arch, &p, None, Some(before), PathBuf::from(&out).as_path())?;
    if !rep.verified {
        return Err("export verification failed: pruning is refused, nothing was deleted".into());
    }
    println!("step 2/4: export verified ({} blobs, {})", rep.blobs, util::human_size(rep.bytes));
    let plan = lifecycle::prune_plan(&p, before)?;
    println!("step 3/4: this would delete {} -> {} versions, freeing about {}",
        plan.versions_before, plan.versions_after,
        util::human_size(plan.bytes_before.saturating_sub(plan.blobs_after.min(plan.blobs_before) as u64)));
    if !confirm_name("Prune after a verified export?", &p.name, a.has("yes")) {
        println!("cancelled; the export stays at {}", rep.out.display());
        return Ok(EXIT_CANCELLED);
    }
    println!("step 4/4: pruning");
    lifecycle::prune(&arch, &mut p, before)?;
    println!("done: the range is exported to {} and the working archive now starts at {}", rep.out.display(), util::fmt_local(before));
    Ok(EXIT_OK)
}

fn cmd_import(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let path = rest.first().ok_or_else(|| "usage: projectlife import <export-dir> [--into P|--new NAME]".to_string())?;
    if a.val("into").is_some() {
        println!("note: --into is refused for a project that already has a journal (history would be applied out of order).");
        println!("      Use --new NAME to import into its own project, or export instead.");
    }
    let into = a.val("into");
    // `--new NAME` names the new project; a bare `--new` (as round 288's usage had it) keeps the
    // exported name, so an empty value must not be treated as a name.
    let new_name = a.val("new").filter(|s| !s.is_empty()).or_else(|| a.val("name"));
    let rep = lifecycle::import(&arch, Path::new(path), into.as_deref(), new_name.as_deref())?;
    println!("imported into project {}: {} blobs added, {} already present, {} events", rep.project, rep.blobs_added, rep.skipped_blobs, rep.events_added);
    ops::record(
        &arch,
        "import",
        &rep.project,
        json!({"from": path, "blobsAdded": rep.blobs_added, "eventsAdded": rep.events_added}),
    );
    Ok(EXIT_OK)
}

fn cmd_archive_delete(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife archive-delete <project>".to_string())?;
    let p = arch.find(name)?;
    let versions = lifecycle::versions_count(&p)?;
    let journal = events::load_journal(&p.dir)?;
    let first = events::first_ts(&journal.events).unwrap_or(0);
    let last = events::last_observed_at(&journal.events).unwrap_or(0);
    println!("Project:  {} ({})", p.name, p.id);
    println!("Path:     {}", p.project_root);
    println!("Versions: {versions}");
    println!("Size:     {}", util::human_size(p.size_on_disk()));
    println!("History:  {} … {}", util::fmt_local(first), util::fmt_local(last));
    println!("This deletes the whole project archive. It is the only irreversible operation.");
    if let Some(dir) = a.val("export-first") {
        let rep = lifecycle::export(&arch, &p, None, None, PathBuf::from(&dir).as_path())?;
        if !rep.verified {
            return Err("export verification failed: deletion refused".into());
        }
        println!("exported first: {} ({} blobs, verified)", rep.out.display(), rep.blobs);
    }
    if !confirm_name("Type the project name to delete its whole history", &p.name, a.has("yes")) {
        println!("cancelled");
        return Ok(EXIT_CANCELLED);
    }
    let size = lifecycle::archive_delete(&arch, &p)?;
    println!("deleted {} ({})", p.name, util::human_size(size));
    Ok(EXIT_OK)
}

fn cmd_check(rest: &[String], a: &Args, verify_alias: bool) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let mut projects: Vec<Project> = if rest.is_empty() {
        arch.load_projects()?
    } else {
        vec![arch.find(&rest[0])?]
    };
    let deep = a.has("deep") || verify_alias;
    let mut results: Vec<Value> = Vec::new();
    let mut worst = EXIT_OK;
    for p in projects.iter_mut() {
        let _ = lifecycle::recover_prune(&arch, p);
        let journal = match events::load_journal(&p.dir) {
            Ok(j) => j,
            Err(e) => {
                println!("{}: JOURNAL ERROR — {e}", p.name);
                println!("Writing to this project stops; nothing is deleted. Inspect it with: python3 recover.py check");
                worst = EXIT_ERR;
                continue;
            }
        };
        let store = Store::new(&p.blobs_dir(), &p.tmp_dir());
        let mut referenced: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for e in &journal.events {
            if events::event_type(e) == "put" {
                if let Some(h) = events::get_str(e, "hash") {
                    *referenced.entry(h).or_insert(0) += 1;
                }
            }
        }
        let mut missing = Vec::new();
        let mut corrupted = Vec::new();
        let mut bytes = 0u64;
        for (h, _n) in &referenced {
            match store.size(h) {
                None => missing.push(h.clone()),
                Some(sz) => {
                    bytes += sz;
                    if deep {
                        if let Err(err) = store.read_verified(h) {
                            corrupted.push((h.clone(), err));
                        }
                    }
                }
            }
        }
        let dangling: Vec<String> = store
            .list_all()
            .into_iter()
            .map(|(h, _)| h)
            .filter(|h| !referenced.contains_key(h))
            .collect();
        if a.json {
            results.push(json!({
                "project": p.name, "versions": referenced.len(), "bytes": bytes,
                "missingBlobs": missing, "corruptedBlobs": corrupted.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(),
                "danglingBlobs": dangling, "trailingPartialLine": journal.trailing_partial,
            }));
        } else {
            println!("{}: {} versions, {}", p.name, referenced.len(), util::human_size(bytes));
            if journal.trailing_partial {
                println!("  note: the last journal line is half-written (ignored, history intact)");
            }
            if missing.is_empty() {
                println!("  blobs referenced by the journal: all present");
            } else {
                println!("  MISSING BLOBS: {} (these versions cannot be restored)", missing.len());
                for h in missing.iter().take(10) {
                    println!("    - {h}");
                }
                worst = EXIT_ERR;
            }
            if deep {
                if corrupted.is_empty() {
                    println!("  deep verification: every blob re-read and matched its name");
                } else {
                    println!("  CORRUPTED BLOBS: {}", corrupted.len());
                    for (h, err) in corrupted.iter().take(10) {
                        println!("    - {h}: {err}");
                    }
                    worst = EXIT_ERR;
                }
            }
            if !dangling.is_empty() {
                println!("  dangling blobs: {} ({}) — safe to remove with --fix", dangling.len(), util::human_size(dangling.len() as u64));
                if a.has("fix") {
                    if confirm(&format!("Delete {} dangling blobs?", dangling.len()), a.has("yes")) {
                        let mut n = 0;
                        for h in &dangling {
                            if std::fs::remove_file(crate::store::blob_path(&p.blobs_dir(), h)).is_ok() {
                                n += 1;
                            }
                            let _ = std::fs::remove_file(p.quarantine_dir().join(h));
                        }
                        println!("  deleted {n} dangling blobs");
                    } else {
                        println!("  cancelled");
                    }
                }
            }
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&Value::Array(results)).unwrap_or_default());
    }
    Ok(worst)
}

fn cmd_rebuild_cache(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let projects = if rest.is_empty() {
        arch.load_projects()?
    } else {
        vec![arch.find(&rest[0])?]
    };
    let mut rebuilt: Vec<Value> = Vec::new();
    for p in &projects {
        let journal = events::load_journal(&p.dir)?;
        let cache = scan::StateCache::from_journal(&journal.events);
        cache.save(p)?;
        if a.json {
            rebuilt.push(json!({"project": p.name, "files": cache.files.len(), "journalEvents": journal.events.len()}));
        } else {
            println!("{}: cache rebuilt from the journal ({} files)", p.name, cache.files.len());
        }
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "rebuilt": rebuilt,
                "note": "the journal is the source of truth; the cache is derived from it and can always be rebuilt",
            }))
            .unwrap_or_default()
        );
    }
    Ok(EXIT_OK)
}

fn cmd_audit_archive(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let update = a.has("update");
    let (count, problems) = doctor::archive_digest(&arch, update)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "archive": arch.root.to_string_lossy(),
                "filesHashed": count,
                "differences": problems,
                "ok": problems.is_empty(),
                "digestWritten": update,
                "note": "a difference means the bytes inside the archive changed outside this program; the archive is observable, not protected against that",
            }))
            .unwrap_or_default()
        );
        return Ok(if problems.is_empty() { EXIT_OK } else { EXIT_ERR });
    }
    println!("archive files hashed: {count}");
    if problems.is_empty() {
        println!("comparison with the previous digest: no changes to existing files");
    } else {
        println!("DIFFERENCES from the previous digest ({}):", problems.len());
        for p in problems.iter().take(50) {
            println!("  - {p}");
        }
        println!("This means data inside the archive was removed or substituted. The archive is not protected against that — it is only observable here.");
    }
    if update {
        println!("new digest written to {}", arch.root.join("manifest").display());
    } else {
        println!("(no digest was written: add --update to record a new one)");
    }
    Ok(if problems.is_empty() { EXIT_OK } else { EXIT_ERR })
}

fn cmd_quarantine(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
    let projects = arch.load_projects()?;
    match sub {
        "list" => {
            let mut n = 0;
            let mut entries: Vec<Value> = Vec::new();
            for p in &projects {
                let dir = p.quarantine_dir();
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        if a.json {
                            let meta = e.metadata().ok();
                            entries.push(json!({
                                "project": p.name,
                                "hash": e.file_name().to_string_lossy(),
                                "bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                                "at": meta.as_ref().and_then(|m| m.modified().ok())
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_millis() as i64),
                            }));
                        } else {
                            println!("{}/{}", p.name, e.file_name().to_string_lossy());
                        }
                        n += 1;
                    }
                }
            }
            if a.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "count": n, "entries": entries,
                        "note": "quarantined bytes are never deleted automatically; `quarantine restore <hash>` puts one back",
                    }))
                    .unwrap_or_default()
                );
                return Ok(EXIT_OK);
            }
            println!("{n} blobs in quarantine (never cleaned automatically)");
        }
        "restore" => {
            let hash = rest.get(1).ok_or_else(|| "usage: projectlife quarantine restore <hash>".to_string())?;
            let mut done = false;
            for p in &projects {
                let src = p.quarantine_dir().join(hash);
                if src.is_file() {
                    let store = Store::new(&p.blobs_dir(), &p.tmp_dir());
                    let data = std::fs::read(&src).map_err(|e| e.to_string())?;
                    let got = crate::store::sha256_bytes(&data);
                    if got != *hash {
                        return Err(format!("the quarantined blob no longer matches its name (sha256 {got})"));
                    }
                    store.put(hash, &data)?;
                    println!("restored into {} (verified sha256)", p.name);
                    done = true;
                }
            }
            if !done {
                return Err(format!("no quarantined blob named {hash}"));
            }
        }
        other => return Err(format!("unknown quarantine subcommand: {other}")),
    }
    Ok(EXIT_OK)
}

fn cmd_scan_once(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let only = if a.has("all") {
        None
    } else {
        rest.first().cloned()
    };
    let res = daemon::run_cycle(&arch, only.as_deref(), "observed", a.has("deep"), a.has("skipped"))?;
    if a.json {
        let items: Vec<Value> = res
            .projects
            .iter()
            .map(|(name, r)| match r {
                Ok(rep) => json!({
                    "project": name, "files": rep.files_on_disk, "created": rep.created,
                    "changed": rep.changed, "deleted": rep.deleted, "moved": rep.moved,
                    "skipped": rep.skipped, "newBlobs": rep.blobs_new, "ms": rep.duration_ms,
                }),
                Err(e) => json!({"project": name, "error": e}),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"archiveState": res.archive_state, "projects": items})).unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    match res.archive_state.as_str() {
        "ARCHIVE_OFFLINE" => {
            println!("ARCHIVE_OFFLINE: the archive root is unreachable; nothing was written.");
            return Ok(EXIT_ERR);
        }
        "ARCHIVE_FULL" => {
            // The reason, with the numbers: "below the stop threshold" is not actionable on its own,
            // and on 2026-10-06 this line was the only thing the owner could see while the daemon
            // was refusing to write.
            println!("ARCHIVE_FULL: {}; storing new versions is stopped.", crate::space::check(&arch).reason);
            return Ok(EXIT_ERR);
        }
        _ => {}
    }
    for (name, r) in &res.projects {
        match r {
            Ok(rep) => println!(
                "{}: {} files, +{} ~{} -{} moved {}, {} new blobs, {} skipped, {} ms",
                name, rep.files_on_disk, rep.created, rep.changed, rep.deleted, rep.moved, rep.blobs_new, rep.skipped, rep.duration_ms
            ),
            Err(e) => println!("{name}: {e}"),
        }
    }
    if a.has("skipped") {
        // The counts here come from counting the skipped directories, which is why this flag is
        // the only place the ordinary cycle is not enough.
        for (name, r) in &res.projects {
            if let Ok(rep) = r {
                println!("{name}: skipped paths ({}):", rep.skipped_paths.len());
                for (p, reason, rule) in rep.skipped_paths.iter().take(200) {
                    println!("  {p}  [{reason}] {rule}");
                }
                if rep.skipped_paths.len() > 200 {
                    println!("  … and {} more", rep.skipped_paths.len() - 200);
                }
            }
        }
    }
    if res.projects.is_empty() {
        println!("no projects to observe");
    }
    println!("heartbeat written; external-timer mode: the promise accuracy equals the timer period.");
    Ok(EXIT_OK)
}

/// `pl partial-pass <project> <path>...` — run exactly the pass a notification would run.
///
/// The paths are the ones `watch_state.json` lists as `lastChangedPaths` (with or without the
/// `project:` prefix, and a directory path means "walk this subtree"). The command exists so a
/// partial pass can be run, timed and inspected from outside the process — it is the measurement
/// surface of the feature, not a second implementation: it calls the same
/// `scan::scan_project_partial` the daemon calls.
fn cmd_partial_pass(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest
        .first()
        .ok_or_else(|| "usage: projectlife partial-pass <project> <path>... [--json]".to_string())?;
    let mut paths: Vec<String> = Vec::new();
    for raw in rest.iter().skip(1) {
        let rel = match raw.split_once(':') {
            Some((n, r)) if n == name => r.to_string(),
            _ => raw.clone(),
        };
        let rel = scan::normalize_rel(&rel);
        if !rel.is_empty() {
            paths.push(rel);
        }
    }
    if paths.is_empty() {
        return Err("usage: projectlife partial-pass <project> <path>... [--json]".into());
    }
    // The same serialisation as any other pass: a partial pass never overlaps the daemon's cycle.
    let lock = arch.lock("partial-pass")?;
    let mut project = arch.find(name)?;
    let started = util::now_ms();
    let rep = scan::scan_project_partial(&arch, &mut project, &paths, "changed");
    lock.release();
    let rep = rep?;
    let elapsed = util::now_ms() - started;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": rep.project,
                "partial": rep.partial,
                "scopePaths": rep.scope_paths,
                "dirsWalked": rep.dirs_walked,
                "filesInScope": rep.files_on_disk,
                "trackedBefore": rep.tracked_before,
                "created": rep.created, "changed": rep.changed, "deleted": rep.deleted,
                "moved": rep.moved, "unchanged": rep.unchanged, "skipped": rep.skipped,
                "newBlobs": rep.blobs_new, "bytesRead": rep.bytes_read,
                "ms": rep.duration_ms, "wallMs": elapsed,
                // Round 294 — what the bookkeeping cost, as numbers rather than as a claim.
                "cacheBytesRead": rep.cache_bytes_read,
                "journalFullBytesRead": rep.journal_full_bytes_read,
                "journalTailBytesRead": rep.journal_tail_bytes_read,
                "skipMapBytesRead": rep.skip_map_bytes_read,
                "cacheBaseRewrites": rep.cache_base_rewrites,
                "cacheDeltaRecords": rep.cache_delta_records,
                "cacheDeltaRecordsBefore": rep.cache_delta_records_before,
                "cacheCompactions": rep.cache_compactions,
                "cacheRebuilt": rep.cache_rebuilt,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!(
        "{}: partial pass over {} notification path(s) — {} director(ies) walked, {} file(s) in scope of {} tracked, +{} ~{} -{} moved {} ({} ms)",
        rep.project, rep.scope_paths, rep.dirs_walked, rep.files_on_disk, rep.tracked_before,
        rep.created, rep.changed, rep.deleted, rep.moved, rep.duration_ms
    );
    println!("the periodic full pass is unaffected by this command: it still runs on its own schedule.");
    Ok(EXIT_OK)
}

fn cmd_drill(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife drill <project>".to_string())?;
    let project = arch.find(name)?;
    let started = util::now_ms();
    // 1. make sure the current state is in the archive
    let mut p2 = project.clone();
    let sopts = ScanOptions {
        reason: "manual".into(),
        deep: true,
        verbose_filters: false,
        dry_run: false,
        with_initial_snapshot: false,
        count_skipped: false,
        scope: None,
    };
    let scan_rep = scan::scan_project(&arch, &mut p2, &sopts)?;
    let moment = util::now_ms();
    // 2. sandbox: an independent copy of what is on disk right now (the "original")
    let sandbox = arch.root.join("tmp").join(format!("drill-{started}"));
    let pristine = sandbox.join("pristine");
    std::fs::create_dir_all(&pristine).map_err(|e| e.to_string())?;
    let filter = FilterConfig::for_profile(&project.profile(), &project.settings());
    let (own, git) = scan::open_ignore_rules(&project.project_path());
    let prefixes = vec![arch.root.clone()];
    let (cur, _skips, _errs) = scan::walk(&project.project_path(), &filter, &prefixes, &own, &git, false);
    let mut expected: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut symlinks_on_disk = 0usize;
    for (rel, cf) in &cur {
        if cf.kind == "symlink" {
            // A symlink is compared as a link, never dereferenced: reading its target would test
            // the target, not the archive.
            symlinks_on_disk += 1;
            continue;
        }
        let abs = project.project_path().join(rel);
        let data = match std::fs::read(&abs) {
            Ok(d) => d,
            Err(_) => continue,
        };
        expected.insert(rel.clone(), crate::store::sha256_bytes(&data));
        let dst = pristine.join(rel);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&dst, &data);
    }
    // 3. restore from the archive into a fresh folder, as after a wipe
    let restored_dir = sandbox.join("restored");
    let ropts = RestoreOptions {
        at: moment,
        at_label: "drill moment".into(),
        paths: Vec::new(),
        to: Some(restored_dir.clone()),
        into_project: false,
        clean: false,
        preview: false,
        missing: false,
    };
    let plan = restore::plan(&arch, &project, &ropts)?;
    let restore_started = util::now_ms();
    let report = restore::execute(&project, &plan, &ropts)?;
    let restore_ms = util::now_ms() - restore_started;
    // 4. compare byte for byte
    let mut checked = 0usize;
    let mut mismatched: Vec<String> = Vec::new();
    let mut absent: Vec<String> = Vec::new();
    for (rel, hash) in &expected {
        let p = restored_dir.join(rel);
        match std::fs::read(&p) {
            Ok(data) => {
                checked += 1;
                if crate::store::sha256_bytes(&data) != *hash {
                    mismatched.push(rel.clone());
                }
            }
            Err(_) => absent.push(rel.clone()),
        }
    }
    let passed = mismatched.is_empty() && absent.is_empty() && report.failed == 0;
    // The rehearsal is recorded twice on purpose: in the archive's operation log (what the program
    // did) and in the project's own settings (when its promise was last tested on this machine).
    {
        if let Ok(mut p3) = arch.find(name) {
            p3.settings_mut().insert("lastDrillAt".into(), Value::from(iso_ms(util::now_ms())));
            let _ = p3.save_meta();
        }
        ops::record(
            &arch,
            "drill",
            &project.name,
            json!({
                "result": if passed { "PASS" } else { "FAIL" },
                "ms": util::now_ms() - started,
                "files": expected.len(),
                "mismatched": mismatched.len(),
                "absent": absent.len(),
            }),
        );
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project.name,
                "moment": moment,
                "filesOnDisk": expected.len(),
                "filesChecked": checked,
                "mismatched": mismatched,
                "absent": absent,
                "restoreMs": restore_ms,
                "totalMs": util::now_ms() - started,
                "missingBlobs": report.failed,
                "verdict": if passed { "PASS" } else { "FAIL" },
            }))
            .unwrap_or_default()
        );
        return Ok(if passed { EXIT_OK } else { EXIT_ERR });
    }
    println!("Drill for {}:", project.name);
    println!("  observation before the drill: {} files read, {} new versions", scan_rep.files_on_disk, scan_rep.created + scan_rep.changed);
    println!("  files on disk at the drill moment: {} ({} symlinks compared as links, not contents)", expected.len(), symlinks_on_disk);
    println!("  restored from the archive: {} files in {restore_ms} ms", report.restored);
    println!("  compared byte for byte: {checked} ok, {} mismatched, {} absent", mismatched.len(), absent.len());
    if !mismatched.is_empty() {
        for m in mismatched.iter().take(10) {
            println!("    mismatch: {m}");
        }
    }
    if !absent.is_empty() {
        for m in absent.iter().take(10) {
            println!("    absent: {m}");
        }
    }
    println!("  VERDICT: {}", if passed { "PASS — the observed state was restored exactly" } else { "FAIL — see above" });
    println!("  sandbox kept for inspection: {}", sandbox.display());
    Ok(if passed { EXIT_OK } else { EXIT_ERR })
}

fn cmd_daemon(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "run" => {
            println!("daemon running in the foreground (Ctrl-C or SIGTERM to stop; SIGHUP re-reads the configuration)");
            let mut arch = arch;
            daemon::run_daemon(&mut arch, if a.has("no-watch") { Some(false) } else { None })?;
            Ok(EXIT_OK)
        }
        "status" => {
            match arch.heartbeat_ms() {
                Some(ts) => {
                    let age = util::now_ms() - ts;
                    println!("last cycle: {} ({} s ago)", util::fmt_local(ts), age / 1000);
                    println!("status: {}", if age < 60_000 { "running" } else { "not running" });
                }
                None => println!("status: no heartbeat — the daemon has never run"),
            }
            match daemon::trigger_state(&arch) {
                Some(st) => {
                    let partial = st.get("partialCycles").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!(
                        "trigger: {} — {} event(s), {} notification cycle(s) ({} partial, {} full), {} periodic cycle(s), {} overflow(s), {} lost",
                        st.get("describe").and_then(|v| v.as_str()).unwrap_or("?"),
                        st.get("events").and_then(|v| v.as_u64()).unwrap_or(0),
                        st.get("triggerCycles").and_then(|v| v.as_u64()).unwrap_or(0),
                        partial,
                        st.get("triggerCycles").and_then(|v| v.as_u64()).unwrap_or(0).saturating_sub(partial),
                        st.get("periodicCycles").and_then(|v| v.as_u64()).unwrap_or(0),
                        st.get("overflows").and_then(|v| v.as_u64()).unwrap_or(0),
                        st.get("lostEvents").and_then(|v| v.as_u64()).unwrap_or(0),
                    );
                    if let Some(lp) = st.get("lastPartial") {
                        if let Some(projects) = lp.get("projects").and_then(|v| v.as_array()) {
                            for p in projects {
                                println!(
                                    "         partial pass: {} path(s) → {} director(ies) walked, {} file(s) in scope, +{} ~{} -{} moved {} ({} ms){}",
                                    p.get("scopePaths").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("dirsWalked").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("filesInScope").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("created").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("changed").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("deleted").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("moved").and_then(|v| v.as_u64()).unwrap_or(0),
                                    p.get("ms").and_then(|v| v.as_i64()).unwrap_or(0),
                                    match p.get("project").and_then(|v| v.as_str()) { Some(n) => format!(" [{n}]"), None => String::new() },
                                );
                            }
                        }
                    }
                    if let Some(p) = st.get("pending").and_then(|v| v.as_u64()) {
                        if p > 0 {
                            println!("         {p} changed path(s) waiting for the debounce");
                        }
                    }
                }
                None => println!("trigger: no trigger state yet (the daemon has not run)"),
            }
            if arch.root.join(".lock").exists() {
                println!("lock: present ({})", std::fs::read_to_string(arch.root.join(".lock")).unwrap_or_default().trim());
            }
            if let Some(h) = arch.daemon_holder() {
                println!("daemon lock: held by \"{}\"", h.raw);
            }
            match std::fs::read_to_string(arch.root.join("config_state.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(v) => {
                    let reloads = v.get("reloads").and_then(|x| x.as_u64()).unwrap_or(0);
                    let when = v.get("atIso").and_then(|x| x.as_str()).unwrap_or("");
                    println!(
                        "configuration: re-read {reloads} time(s) without a restart (last: {when}, {})",
                        v.get("reason").and_then(|x| x.as_str()).unwrap_or("start")
                    );
                }
                None => println!("configuration: read when the process started (no re-read recorded yet)"),
            }
            Ok(EXIT_OK)
        }
        "install" => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let interval = a.val("interval").unwrap_or_else(|| arch.config.str_of("intervalSeconds", "5"));
            #[allow(unused_mut)]
            let mut written: Vec<PathBuf> = Vec::new();
            #[cfg(target_os = "macos")]
            {
                let plist = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>com.projectlife.daemon</string>\n<key>ProgramArguments</key><array><string>{}</string><string>daemon</string><string>run</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n</dict></plist>\n",
                    exe.display()
                );
                let p = PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join("Library/LaunchAgents/com.projectlife.daemon.plist");
                util::write_atomic(&p, plist.as_bytes()).map_err(|e| e.to_string())?;
                written.push(p);
            }
            #[cfg(target_os = "linux")]
            {
                let unit = format!(
                    "[Unit]\nDescription=Project Life observer\n\n[Service]\nExecStart={} daemon run\nRestart=always\nRestartSec=5\nNice=10\nIOSchedulingClass=idle\n\n[Install]\nWantedBy=default.target\n",
                    exe.display()
                );
                let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/systemd/user");
                let p = dir.join("projectlife.service");
                util::write_atomic(&p, unit.as_bytes()).map_err(|e| e.to_string())?;
                written.push(p);
                let timer = format!(
                    "# Alternative to the resident daemon: run one observation cycle on a timer.\n# A short-lived process cannot be killed and forgotten like a daemon can.\n[Unit]\nDescription=Project Life observation cycle (external timer mode)\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec={}s\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n",
                    interval
                );
                let tp = dir.join("projectlife-timer.service");
                let svc = format!("[Unit]\nDescription=Project Life one-shot observation\n\n[Service]\nType=oneshot\nExecStart={} scan-once --all\nNice=10\nIOSchedulingClass=idle\n", exe.display());
                util::write_atomic(&tp, svc.as_bytes()).map_err(|e| e.to_string())?;
                let tu = dir.join("projectlife.timer");
                util::write_atomic(&tu, timer.as_bytes()).map_err(|e| e.to_string())?;
                written.push(tu);
            }
            #[cfg(windows)]
            {
                // Windows has a scheduler (Task Scheduler) and this build cannot test a change to
                // it, so the exact commands are printed and nothing is executed. `daemon start`
                // (detached) is the route this build does implement.
                let _ = interval;
                println!("on Windows, autostart is a decision and not a side effect of this command.");
                println!("Start it now:      projectlife --archive \"{}\" daemon start", arch.root.display());
                println!("At every login:    schtasks /create /tn ProjectLifeDaemon /sc onlogon /rl limited \\");
                println!("                     /tr \"\"{}\" --archive \"{}\" daemon run\"", exe.display(), arch.root.display());
                println!("Remove it again:   schtasks /delete /tn ProjectLifeDaemon /f");
                println!("Nothing above was executed by this program.");
            }
            for w in &written {
                println!("wrote {}", w.display());
            }
            println!("enable the resident daemon:  systemctl --user enable --now projectlife.service   (Linux)");
            println!("or use the timer instead:    systemctl --user enable --now projectlife.timer     (Linux)");
            println!("macOS: launchctl load -w ~/Library/LaunchAgents/com.projectlife.daemon.plist");
            Ok(EXIT_OK)
        }
        "uninstall" => {
            #[cfg(target_os = "linux")]
            {
                let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/systemd/user");
                for f in ["projectlife.service", "projectlife-timer.service", "projectlife.timer"] {
                    let p = dir.join(f);
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                        println!("removed {}", p.display());
                    }
                }
            }
            #[cfg(target_os = "macos")]
            {
                let p = PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join("Library/LaunchAgents/com.projectlife.daemon.plist");
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                    println!("removed {}", p.display());
                }
            }
            Ok(EXIT_OK)
        }
        "start" | "stop" => {
            let action = if sub == "start" { "start" } else { "stop" };
            #[cfg(target_os = "linux")]
            {
                let st = std::process::Command::new("systemctl")
                    .args(["--user", action, "projectlife.service"])
                    .status();
                match st {
                    Ok(s) if s.success() => println!("systemctl --user {action} projectlife.service: ok"),
                    _ => println!("systemctl is unavailable here. Start it directly: projectlife daemon run"),
                }
            }
            #[cfg(target_os = "macos")]
            {
                let cmd = if sub == "start" { "load" } else { "unload" };
                let _ = std::process::Command::new("launchctl")
                    .args([cmd, "-w", "~/Library/LaunchAgents/com.projectlife.daemon.plist"])
                    .status();
                println!("launchctl {cmd} issued");
            }
            #[cfg(windows)]
            {
                // Windows: a process with no console and no signals. `start` spawns the daemon
                // detached (it outlives this shell and cannot be killed by closing a window), and
                // `stop` writes the request file the daemon reads — never TerminateProcess first.
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                if action == "start" {
                    if arch.daemon_holder().is_some() {
                        println!("a daemon is already observing this archive ({}); nothing started", arch.root.display());
                    } else {
                        use std::os::windows::process::CommandExt;
                        const DETACHED_PROCESS: u32 = 0x0000_0008;
                        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
                        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                        let mut cmd = std::process::Command::new(&exe);
                        cmd.arg("--archive").arg(&arch.root).arg("daemon").arg("run");
                        cmd.stdin(std::process::Stdio::null());
                        cmd.stdout(std::process::Stdio::null());
                        cmd.stderr(std::process::Stdio::null());
                        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
                        match cmd.spawn() {
                            Ok(c) => println!(
                                "daemon started detached (pid {}); it keeps observing when this program exits",
                                c.id()
                            ),
                            Err(e) => println!("could not start the daemon: {e}"),
                        }
                    }
                }
            }
            if sub == "stop" {
                // Every platform, with or without systemd/launchd: ask the holder by name, wait for
                // it to actually leave, and only then say the word "stopped".
                match arch.daemon_holder() {
                    Some(info) => match info.pid {
                        Some(pid) => {
                            match arch.request_stop(pid) {
                                Ok(p) => println!("stop request written for pid {pid}: {}", p.display()),
                                Err(e) => println!("could not write the stop request: {e}"),
                            }
                            // Two independent answers, and either one is enough: the pid is gone,
                            // or the daemon-lifetime lock is. The second is not redundant — on unix a
                            // child that has exited but has not been reaped yet still answers
                            // `kill(pid, 0)` with success, and a daemon whose parent never waited
                            // would then look alive for ever. The lock is removed by the daemon
                            // itself on its way out, so it is the daemon's own word, not ours.
                            let deadline = util::now_ms() + 15_000;
                            let lock_path = arch.root.join(".daemon");
                            let mut gone = false;
                            while util::now_ms() < deadline {
                                if !crate::archive::pid_alive(pid) || !lock_path.exists() {
                                    gone = true;
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(200));
                            }
                            if gone {
                                println!("daemon (pid {pid}) stopped");
                            } else {
                                #[cfg(windows)]
                                {
                                    println!("daemon (pid {pid}) has not stopped within 15 s; terminating it");
                                    match crate::util::win::terminate(pid as u32) {
                                        Ok(()) => println!("terminated pid {pid} (the request was ignored)"),
                                        Err(e) => println!("could not terminate pid {pid}: {e}"),
                                    }
                                }
                                #[cfg(not(windows))]
                                {
                                    println!(
                                        "daemon (pid {pid}) has not stopped within 15 s — it may be in the middle of a \
                                         pass; ask again, or (if you are sure) kill {pid}, or: projectlife doctor --fix-lock"
                                    );
                                }
                            }
                        }
                        None => println!(
                            "the daemon lock names no pid (\"{}\"), so a stop request cannot name it either; \
                             projectlife doctor --fix-lock removes a lock whose owner is gone",
                            info.raw
                        ),
                    },
                    None => println!("no daemon is observing this archive (nothing to stop)"),
                }
            }
            Ok(EXIT_OK)
        }
        other => Err(format!("unknown daemon subcommand: {other} (run|install|uninstall|start|stop|status)")),
    }
}

fn cmd_config(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("get");
    let mut cfg: Config = arch.config.clone();
    match sub {
        "get" => {
            let key = rest.get(1);
            match key {
                Some(k) => match cfg.map.get(k.as_str()) {
                    Some(v) => {
                        if a.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&json!({"key": k, "value": v})).unwrap_or_default()
                            );
                            return Ok(EXIT_OK);
                        }
                        println!("{k} = {v}")
                    }
                    None => return Err(format!("no such key: {k}")),
                },
                None => {
                    if a.json {
                        let m: Map<String, Value> = cfg.map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        println!("{}", serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default());
                        return Ok(EXIT_OK);
                    }
                    for (k, v) in &cfg.map {
                        println!("{k} = {v}");
                    }
                }
            }
        }
        "set" => {
            let key = rest.get(1).ok_or_else(|| "usage: projectlife config set <key> <value>".to_string())?;
            let raw = rest.get(2).ok_or_else(|| "usage: projectlife config set <key> <value>".to_string())?;
            let value: Value = if let Ok(n) = raw.parse::<i64>() {
                Value::from(n)
            } else if let Ok(f) = raw.parse::<f64>() {
                Value::from(f)
            } else if raw == "true" || raw == "false" {
                Value::from(raw == "true")
            } else {
                Value::from(raw.clone())
            };
            cfg.set(key, value);
            cfg.save(&arch.root).map_err(|e| e.to_string())?;
            println!("{key} = {}", cfg.map.get(key.as_str()).cloned().unwrap_or(Value::Null));
        }
        other => return Err(format!("unknown config subcommand: {other} (get|set)")),
    }
    Ok(EXIT_OK)
}

/// FR-LIF-3: before a project is read or written, an interrupted prune is finished or rolled back.
fn heal_prune(arch: &Archive, project: &mut Project) {
    if let Ok(Some(what)) = lifecycle::recover_prune(arch, project) {
        println!("note: {what}");
    }
}

fn cmd_recover(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife recover <project>".to_string())?;
    let mut p = arch.find(name)?;
    let action = lifecycle::recover_prune(&arch, &mut p)?;
    if a.json {
        let j = events::load_journal(&p.dir)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name,
                "action": action.clone().unwrap_or_else(|| "nothing to recover (no interrupted prune)".into()),
                "recovered": action.is_some(),
                "journalEvents": j.events.len(),
                "historyStartsAt": p.history_starts_at(),
                "historyStartsAtLocal": util::fmt_local(p.history_starts_at()),
                "lastObservedAt": events::last_observed_at(&j.events),
                "trailingPartialLine": j.trailing_partial,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    match action {
        Some(what) => println!("{}: {what}", p.name),
        None => println!("{}: nothing to recover (no interrupted prune)", p.name),
    }
    // The point of the recovery is a usable journal, so say plainly whether there is one.
    let j = events::load_journal(&p.dir)?;
    println!(
        "journal readable: {} events, history starts at {}, last observed {}",
        j.events.len(),
        util::fmt_local(p.history_starts_at()),
        events::last_observed_at(&j.events).map(util::fmt_local).unwrap_or_else(|| "never".into())
    );
    if j.trailing_partial {
        println!("note: the last journal line is half-written (ignored; history intact)");
    }
    Ok(EXIT_OK)
}

fn cmd_doctor(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    if a.has("fix-lock") {
        match arch.break_lock(a.has("force")) {
            Ok(msg) => println!("lock: {msg}"),
            Err(e) => return Err(e),
        }
    }
    let checks = doctor::doctor(&arch);
    let mut worst = EXIT_OK;
    if a.json {
        let items: Vec<Value> = checks
            .iter()
            .map(|c| json!({"level": c.level, "text": c.text, "fix": c.fix}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_default());
    } else {
        for c in &checks {
            println!("{:<5} {}", c.level, c.text);
            if let Some(f) = &c.fix {
                println!("      fix: {f}");
            }
        }
    }
    for c in &checks {
        if c.level == ERROR {
            worst = EXIT_ERR;
        } else if c.level == WARN && worst == EXIT_OK {
            worst = EXIT_OK;
        }
    }
    let errors = checks.iter().filter(|c| c.level == ERROR).count();
    let warnings = checks.iter().filter(|c| c.level == WARN).count();
    if !a.json {
        println!("{errors} error(s), {warnings} warning(s), {} ok", checks.iter().filter(|c| c.level == OK).count());
    }
    Ok(worst)
}


// ---------------------------------------------------------------------------------------------
// Round 292 — the daily surface.
//
// Two rules hold for everything below. First: a command that changes the project folder or the
// archive is a user action and says so; the advisory commands (`undo`, `suggest`, `last-good`,
// `panic` without `--yes`) print the exact command instead of running it. Second: every command
// that reads is safe to run anywhere, which is what lets the MCP server reuse the same functions.
// ---------------------------------------------------------------------------------------------

use crate::health;
use crate::mcp;
use crate::ops;
use crate::quick;
use crate::retention;

/// One line per event, built from the fields the event actually carries.
fn ev_one_line(e: &Ev) -> String {
    let keys = [
        "path", "from", "to", "target", "hash", "size", "files", "deleted", "changed", "created", "kind", "reason",
        "rule", "label", "policy",
    ];
    let mut parts: Vec<String> = Vec::new();
    for k in keys {
        if let Some(v) = e.get(k) {
            if v.is_null() {
                continue;
            }
            let s = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if s.is_empty() {
                continue;
            }
            if k == "hash" && s.len() > 12 {
                parts.push(format!("hash={}…", &s[..12]));
            } else {
                parts.push(format!("{k}={s}"));
            }
        }
    }
    parts.join(" ")
}

fn max_age_ms(a: &Args) -> i64 {
    a.val("max-age").and_then(|s| s.parse::<i64>().ok()).unwrap_or(health::DEFAULT_MAX_AGE_SECONDS).max(1) * 1000
}

/// `heartbeat-check` — exit 0 fresh, 1 stale, 2 stale while a daemon claims to be running.
///
/// The exit code is the whole point: the shell decides who to tell, and `pl` never opens a socket.
fn cmd_heartbeat_check(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let (code, hb) = health::heartbeat_exit(&arch, max_age_ms(a));
    if a.json {
        println!("{}", serde_json::to_string_pretty(&hb.to_json()).unwrap_or_default());
    } else {
        match hb.age_ms {
            Some(age) => println!(
                "heartbeat: {} s old (limit {} s), mode: {}, last cycle {}",
                age / 1000,
                hb.max_age_ms / 1000,
                hb.mode(),
                hb.ts.map(util::fmt_local).unwrap_or_default()
            ),
            None => println!("heartbeat: none — nothing has ever been observed in this archive"),
        }
        if code != 0 {
            if hb.storage.stop {
                // The remedy has to be one that can work. On 2026-10-06 this line advised
                // "pl scan-once --all" on a full disk, where that command cannot write anything
                // either, while the daemon was alive and refusing every write for exactly this
                // reason.
                println!("nothing has been recorded recently, and the reason is not the scheduler: writing is stopped");
                println!("writing stopped: {}", hb.storage.reason);
                match &hb.daemon {
                    Some(who) => println!("a daemon is running ({who}) and is refusing to write until there is room"),
                    None => println!("no daemon is running either"),
                }
                println!(
                    "fix: free space on the volume holding {} (nothing is lost: a pass that cannot store does not advance the state, so the next one records the change)",
                    arch.root.display()
                );
            } else {
                println!("nothing has been recorded recently; the promise is not being kept right now");
                match &hb.daemon {
                    Some(who) => println!("a daemon claims to be running ({who}) — check: pl daemon status"),
                    None => println!("fix: pl scan-once --all   (or a timer: pl daemon install --timer 5)"),
                }
            }
        }
    }
    Ok(code)
}

/// `healthcheck` — doctor's findings reduced to one exit code for a scheduler.
fn cmd_healthcheck(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let checks = health::health_checks(&arch, max_age_ms(a));
    let strict = a.has("strict");
    if a.json {
        let items: Vec<Value> = checks
            .iter()
            .map(|c| json!({"level": c.level, "text": c.text, "fix": c.fix}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_default());
    } else {
        let problems: Vec<&doctor::Check> = checks.iter().filter(|c| c.level != OK).collect();
        for c in &problems {
            println!("{:<5} {}", c.level, c.text);
            if let Some(f) = &c.fix {
                println!("      fix: {f}");
            }
        }
        if problems.is_empty() {
            println!("OK    every check passed ({} checks)", checks.len());
        } else {
            println!("{} problem(s) of {} checks", problems.len(), checks.len());
        }
    }
    Ok(health::worst_exit(&checks, strict))
}

/// Print the retention plan as a table: what stays, what goes, how much comes back.
fn print_retention_plan(plan: &retention::RetentionPlan, dry_run: bool) {
    println!("Policy:   {}", plan.policy.raw);
    println!("Now:      {}", util::fmt_local(plan.now));
    println!();
    println!("{:<14} {:<9} {:>8} {:>7} {:>8}", "AGE WINDOW", "KEEP", "BUCKETS", "KEPT", "DROPPED");
    for w in &plan.windows {
        let label = if w.from_days == 0 { format!("0-{}d", w.to_days) } else { format!("{}d-{}d", w.from_days, w.to_days) };
        println!("{:<14} {:<9} {:>8} {:>7} {:>8}", label, w.unit.label(), w.buckets, w.kept, w.dropped);
    }
    println!();
    println!(
        "Versions: {} -> {} kept ({} dropped)",
        plan.versions_before,
        plan.versions_kept,
        plan.versions_dropped()
    );
    println!(
        "Blobs:    {} -> {} ({})",
        plan.blobs_before,
        plan.blobs_after,
        util::human_size(plan.bytes_saved())
    );
    println!(
        "Size:     {} -> {}",
        util::human_size(plan.bytes_before),
        util::human_size(plan.bytes_after)
    );
    println!("History will start at: {}", util::fmt_local(plan.new_history_starts_at));
    let bucket_anchors: Vec<i64> = plan.anchors.iter().copied().filter(|t| *t != plan.boundary_anchor).collect();
    println!("Retained moments ({}):", bucket_anchors.len());
    for t in bucket_anchors.iter().take(24) {
        println!("  {}", util::fmt_local(*t));
    }
    if bucket_anchors.len() > 24 {
        println!("  … and {} more", bucket_anchors.len() - 24);
    }
    if bucket_anchors.is_empty() {
        println!("  (none: every version inside the policy window is inside the keep-everything window)");
    }
    println!(
        "Everything from {} onwards is kept exactly as it is (that moment is an anchor as well).",
        util::fmt_local(plan.boundary_anchor)
    );
    println!(
        "Between two retained moments the state is the newer one carried forward: the versions in\n\
         between are gone, and the program will not pretend otherwise."
    );
    if dry_run {
        println!("dry run: nothing was changed");
    }
}

/// `prune --policy` and `retention apply`: one entry point for both, so there is exactly one place
/// in the program that applies a policy.
fn apply_policy(arch: &Archive, name: &str, spec: &str, a: &Args, dry_run: bool) -> Result<i32, String> {
    let policy = retention::Policy::parse(spec)?;
    let mut p = arch.find(name)?;
    heal_prune(arch, &mut p);
    let now = util::now_ms();
    let plan = retention::plan(&p, &policy, now)?;
    if a.json {
        let mut v = json!({
            "project": p.name,
            "mode": "policy",
            "policy": policy.raw,
            "now": now,
            "nowLocal": util::fmt_local(now),
            "versionsBefore": plan.versions_before,
            "versionsKept": plan.versions_kept,
            "versionsAfter": plan.versions_kept,
            "dropped": plan.versions_dropped(),
            "blobsBefore": plan.blobs_before,
            "blobsAfter": plan.blobs_after,
            "bytesBefore": plan.bytes_before,
            "bytesAfter": plan.bytes_after,
            "bytesSaved": plan.bytes_saved(),
            "retainedMoments": plan.anchors.iter().filter(|t| **t != plan.boundary_anchor).count(),
            "newHistoryStartsAt": plan.new_history_starts_at,
            "newHistoryStartsAtLocal": util::fmt_local(plan.new_history_starts_at),
            "windows": plan.windows.iter().map(|w| json!({
                "fromDays": w.from_days, "toDays": w.to_days, "keep": w.unit.label(),
                "buckets": w.buckets, "kept": w.kept, "dropped": w.dropped,
            })).collect::<Vec<Value>>(),
            "dryRun": dry_run,
            "applied": false,
        });
        if dry_run {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            return Ok(EXIT_OK);
        }
        if !confirm_name("Apply this retention policy to the history?", &p.name, a.has("yes")) {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            return Ok(EXIT_CANCELLED);
        }
        let outcome = retention::apply(arch, &mut p, &policy, now)?;
        v["applied"] = json!(true);
        v["dryRun"] = json!(false);
        v["removedBlobs"] = json!(outcome.removed_blobs);
        v["blobsAfter"] = json!(outcome.plan.blobs_after);
        v["versionsAfter"] = json!(outcome.plan.versions_kept);
        v["versionsKept"] = json!(outcome.plan.versions_kept);
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    println!("Project:  {}", p.name);
    print_retention_plan(&plan, dry_run);
    if dry_run {
        return Ok(EXIT_OK);
    }
    if !confirm_name("Apply this retention policy to the history?", &p.name, a.has("yes")) {
        println!("cancelled");
        return Ok(EXIT_CANCELLED);
    }
    let outcome = retention::apply(arch, &mut p, &policy, now)?;
    println!(
        "applied: versions {} -> {}, blobs {} -> {} ({} deleted)",
        outcome.plan.versions_before,
        outcome.plan.versions_kept,
        outcome.plan.blobs_before,
        outcome.plan.blobs_after,
        outcome.removed_blobs
    );
    println!(
        "The history now starts at {}. The policy is stored in project.json (settings.retentionPolicy)\n\
         and is NOT applied automatically: it is a note for you, not an instruction to the program.",
        util::fmt_local(outcome.plan.new_history_starts_at)
    );
    Ok(EXIT_OK)
}

/// `retention <project> [<policy> | apply | clear]` — show, store, apply or forget a policy.
fn cmd_retention(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife retention <project> [<policy>|apply|clear]".to_string())?;
    let mut p = arch.find(name)?;
    heal_prune(&arch, &mut p);
    let arg = rest.get(1).map(|s| s.as_str()).unwrap_or("");
    match arg {
        "" => {
            if a.json {
                let spec = retention::stored_policy(&p);
                let applied = retention::applied_at(&p);
                let (plan, plan_error) = match &spec {
                    Some(raw) => match retention::Policy::parse(raw) {
                        Ok(pol) => match retention::plan(&p, &pol, util::now_ms()) {
                            Ok(pl) => (Some(pl), Value::Null),
                            Err(e) => (None, json!(e)),
                        },
                        Err(e) => (None, json!(e)),
                    },
                    None => (None, Value::Null),
                };
                let stored = spec.is_some();
                let v = json!({
                    "project": p.name,
                    "policy": spec.clone(),
                    "stored": stored,
                    "appliedAt": applied.clone(),
                    "appliedAtLocal": applied.as_deref().and_then(|s| archive::parse_iso_ms(s)).map(util::fmt_local),
                    "plan": plan.as_ref().map(|pl| json!({
                        "versionsBefore": pl.versions_before,
                        "versionsKept": pl.versions_kept,
                        "dropped": pl.versions_dropped(),
                        "blobsBefore": pl.blobs_before,
                        "blobsAfter": pl.blobs_after,
                        "bytesBefore": pl.bytes_before,
                        "bytesAfter": pl.bytes_after,
                        "bytesSaved": pl.bytes_saved(),
                        "retainedMoments": pl.anchors.iter().filter(|t| **t != pl.boundary_anchor).count(),
                        "newHistoryStartsAt": pl.new_history_starts_at,
                        "newHistoryStartsAtLocal": util::fmt_local(pl.new_history_starts_at),
                        "windows": pl.windows.iter().map(|w| json!({
                            "fromDays": w.from_days, "toDays": w.to_days, "keep": w.unit.label(),
                            "buckets": w.buckets, "kept": w.kept, "dropped": w.dropped,
                        })).collect::<Vec<Value>>(),
                    })),
                    "planError": plan_error,
                    "note": "a stored policy deletes nothing by itself; it is read only when a prune is asked for",
                });
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                return Ok(EXIT_OK);
            }
            match retention::stored_policy(&p) {
                Some(spec) => {
                    println!("{}: stored policy \"{spec}\"", p.name);
                    match retention::applied_at(&p) {
                        Some(t) => println!(
                            "last applied: {}",
                            util::fmt_local(archive::parse_iso_ms(&t).unwrap_or(0))
                        ),
                        None => println!("last applied: never (stored, not applied)"),
                    }
                    println!("estimate for this policy now:");
                    let policy = retention::Policy::parse(&spec)?;
                    let plan = retention::plan(&p, &policy, util::now_ms())?;
                    print_retention_plan(&plan, true);
                    println!("apply it: pl prune {} --policy \"{spec}\"   (add --yes in a script)", p.name);
                }
                None => {
                    println!("{}: no retention policy stored.", p.name);
                    println!("A policy is a sentence about what to keep, for example:");
                    println!("  pl retention {} \"7d:all,30d:1/day,365d:1/month\"", p.name);
                    println!("Storing it deletes nothing. Applying it is `pl prune {} --policy \"<policy>\"`.", p.name);
                }
            }
            Ok(EXIT_OK)
        }
        "clear" => {
            retention::clear_policy(&mut p)?;
            println!("{}: retention policy forgotten (nothing was deleted)", p.name);
            Ok(EXIT_OK)
        }
        "apply" => {
            let spec = retention::stored_policy(&p)
                .ok_or_else(|| format!("{}: no policy is stored; set one first: pl retention {} \"7d:all,30d:1/day\"", p.name, p.name))?;
            apply_policy(&arch, &p.name, &spec, a, a.has("dry-run"))
        }
        spec => {
            let policy = retention::Policy::parse(spec)?;
            retention::store_policy(&mut p, &policy)?;
            println!("{}: policy stored: {}", p.name, policy.raw);
            for s in &policy.segments {
                println!("  younger than {} days: keep {}", s.days, s.unit.label());
            }
            println!("Nothing was deleted and nothing will happen by itself — a stored policy is read only when you ask for it:");
            println!("  pl prune {} --policy \"{}\" --dry-run", p.name, policy.raw);
            Ok(EXIT_OK)
        }
    }
}

/// `recent` — the last few things that matter, each with the command that follows from it.
fn cmd_recent(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let limit = a.val("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(5).clamp(1, 50);
    let rows = quick::recent(&arch, limit)?;
    if a.json {
        let items: Vec<Value> = rows
            .iter()
            .map(|r| json!({"project": r.project, "at": iso_ms(r.ts), "kind": r.kind, "detail": r.detail, "command": r.command}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    if rows.is_empty() {
        println!("nothing notable yet: no mass event, no gap, no mark, no restore");
        println!("(ordinary versions are not listed here — they are in `pl log <project>`)");
        return Ok(EXIT_OK);
    }
    for r in rows.iter().take(limit * 3) {
        println!(
            "{}  {:<14} {:<14} {}",
            util::fmt_local(r.ts),
            if r.project.is_empty() { "-" } else { &r.project },
            r.kind,
            r.detail
        );
        if let Some(c) = &r.command {
            println!("       -> {c}");
        }
    }
    Ok(EXIT_OK)
}

/// `snap [label]` — a mark with a default label: one keystroke before something risky.
fn cmd_snap(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife snap <project> [label]".to_string())?;
    let p = arch.find(name)?;
    let label = rest
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("snap {}", util::fmt_local(util::now_ms())));
    let mut e = events::ev_new(0, util::now_ms(), "mark");
    events::put_str(&mut e, "label", &label);
    let mut v = vec![e];
    events::append(&p, &mut v)?;
    let at = util::now_ms();
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name, "label": label, "at": at, "atIso": iso_ms(at), "atLocal": util::fmt_local(at)
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("{}: mark \"{label}\" saved — you can always come back to this moment", p.name);
    println!("  restore it:  pl restore {} --mark \"{}\" --to ../{}-recovered", p.name, label, p.name);
    Ok(EXIT_OK)
}

/// `undo` — what to do about the last write this program made to your project folder. Prints the
/// command; never runs it.
fn cmd_undo(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife undo <project>".to_string())?;
    let p = arch.find(name)?;
    match ops::last_of(&arch, "restore", Some(&p.name)) {
        None => {
            println!("{}: no restore has been recorded, so there is nothing to undo.", p.name);
            if let Some(pr) = ops::last_of(&arch, "prune", Some(&p.name)) {
                println!(
                    "The last prune ({}) cannot be undone: the versions it removed are gone. That is why prune asks for confirmation and prints what it will remove first.",
                    util::fmt_local(ops::ts_of(&pr))
                );
            }
            println!("Observation itself never changes your files: nothing else here needs undoing.");
            Ok(EXIT_OK)
        }
        Some(v) => {
            let at = ops::ts_of(&v);
            let into = v.get("intoProject").and_then(|x| x.as_bool()).unwrap_or(false);
            let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("?").to_string();
            if !into {
                println!("The last restore ({}) did NOT touch your project folder — it wrote a separate copy:", util::fmt_local(at));
                println!("  {target}");
                println!("If you do not want it, delete it yourself:  rm -rf \"{target}\"");
                return Ok(EXIT_OK);
            }
            let journal = events::load_journal(&p.dir)?;
            let before = journal.events.iter().filter(|e| events::ts_of(e) < at).map(events::ts_of).max();
            println!("The last write to your project folder was a restore at {}.", util::fmt_local(at));
            match before {
                None => {
                    println!("There is no observation before it, so the state that was there before cannot be rebuilt.");
                }
                Some(ts) => {
                    println!("The state that was there before it is the state at {} — still in the archive.", util::fmt_local(ts));
                    println!("Look first:");
                    println!("  pl diff {} --at \"{}\" --to \"{}\"", p.name, util::fmt_local(ts), util::fmt_local(at));
                    println!("Then, if you mean it (this replaces files in the project folder and asks for confirmation):");
                    println!("  pl restore {} --at \"{}\" --into-project --clean --yes", p.name, util::fmt_local(ts));
                }
            }
            Ok(EXIT_OK)
        }
    }
}

/// `watch` — new events as they are recorded, like tail -f but for the journal.
fn cmd_watch(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife watch <project> [--type mass,gap] [--for SEC]".to_string())?;
    let p = arch.find(name)?;
    let interval = a.val("interval").and_then(|s| s.parse::<u64>().ok()).unwrap_or(500).clamp(50, 60_000);
    let for_ms = a.val("for").and_then(|s| s.parse::<i64>().ok()).map(|s| s * 1000);
    let types: Vec<String> = a
        .val("type")
        .map(|t| t.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let mut seen = events::load_journal(&p.dir)?.events.last().map(events::seq_of).unwrap_or(0);
    println!("watching {} (new events only, Ctrl-C to stop)", p.name);
    let started = util::now_ms();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(interval));
        let journal = events::load_journal(&p.dir)?;
        let fresh: Vec<Ev> = journal.events.iter().filter(|e| events::seq_of(e) > seen).cloned().collect();
        for e in &fresh {
            seen = seen.max(events::seq_of(e));
            if !types.is_empty() && !types.iter().any(|t| t == events::event_type(e)) {
                continue;
            }
            println!(
                "{:>6}  {}  {:<9} {}",
                events::seq_of(e),
                util::fmt_local(events::ts_of(e)),
                events::event_type(e),
                ev_one_line(e)
            );
        }
        if let Some(f) = for_ms {
            if util::now_ms() - started >= f {
                break;
            }
        }
    }
    Ok(EXIT_OK)
}

/// `open` — where the folders are. Prints the path; `--launch` hands it to the file manager.
fn cmd_open(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let target = if a.has("archive") {
        arch.root.clone()
    } else {
        let name = rest.first().ok_or_else(|| "usage: projectlife open <project> [--archive] [--restored DIR]".to_string())?;
        let p = arch.find(name)?;
        match a.val("restored") {
            Some(d) => PathBuf::from(d),
            None => p.project_path(),
        }
    };
    println!("{}", target.display());
    if a.has("launch") {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        match std::process::Command::new(opener).arg(&target).status() {
            Ok(_) => println!("opened with {opener}"),
            Err(e) => println!("could not run {opener}: {e} (the path above is what you wanted)"),
        }
    } else {
        println!("(pass --launch to hand it to {})", if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    }
    Ok(EXIT_OK)
}

/// `since` — everything that changed since the last mark (or last good moment, or an hour ago).
fn cmd_since(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife since <project> [--at T] [--path P] [--stat]".to_string())?;
    let p = arch.find(name)?;
    let journal = events::load_journal(&p.dir)?;
    let now = util::now_ms();
    let (since, label) = if let Some(spec) = a.val("at") {
        (util::parse_at(&spec, now)?, spec)
    } else if let Some(m) = journal
        .events
        .iter()
        .filter(|e| events::event_type(e) == "mark")
        .max_by_key(|e| events::ts_of(e))
    {
        (
            events::ts_of(m),
            format!("last mark \"{}\"", events::get_str(m, "label").unwrap_or_default()),
        )
    } else if let Some((ts, why)) = restore::last_good(&journal.events, &p) {
        (ts, format!("last good ({why})"))
    } else {
        (now - 3_600_000, "an hour ago".to_string())
    };
    let upto = events::last_observed_at(&journal.events).unwrap_or(now);
    let path_filter = a.val("path").map(|x| util::norm_rel(&x));
    let before = events::state_at(&journal.events, since, None);
    let after = events::state_at(&journal.events, upto, None);
    let d = quick::diff_states(&before, &after, path_filter.as_deref());
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": p.name, "since": iso_ms(since), "sinceLabel": label, "upto": iso_ms(upto),
                "added": d.added, "removed": d.removed, "changed": d.changed,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("{}: since {} ({}), up to {}", p.name, util::fmt_local(since), label, util::fmt_local(upto));
    println!("  +{} added, -{} removed, ~{} changed", d.added.len(), d.removed.len(), d.changed.len());
    if !a.has("stat") {
        for k in &d.added {
            println!("A {k}");
        }
        for k in &d.removed {
            println!("D {k}");
        }
        for k in &d.changed {
            println!("M {k}");
        }
    }
    Ok(EXIT_OK)
}

/// `cat` — one file's bytes at one moment, straight from the archive.
fn cmd_cat(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife cat <project> --path P [--at T] [--out FILE]".to_string())?;
    let p = arch.find(name)?;
    let path = a.val("path").ok_or_else(|| "cat requires --path <file>".to_string())?;
    let journal = events::load_journal(&p.dir)?;
    let at = match a.val("at") {
        Some(spec) => util::parse_at(&spec, util::now_ms())?,
        None => events::last_observed_at(&journal.events)
            .ok_or_else(|| format!("{}: nothing has been observed yet", p.name))?,
    };
    let (data, ts, hash) = quick::cat_version(&p, &path, at)?;
    // --json is for a program that has to *show* the bytes, not pipe them: the window asks for a
    // file at a moment and has to draw it. The cap keeps a 2 GB log file from being sent whole.
    if a.json {
        const CAP: usize = 256 * 1024;
        let shown = data.len().min(CAP);
        let text = std::str::from_utf8(&data[..shown]).ok().map(|s| s.to_string());
        let v = json!({
            "path": util::norm_rel(&path),
            "at": ts,
            "atIso": iso_ms(ts),
            "atLocal": util::fmt_local(ts),
            "bytes": data.len(),
            "sha256": hash,
            "encoding": if text.is_some() { "utf8" } else { "binary" },
            "text": text,
            "shownBytes": shown,
            "truncated": data.len() > shown,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    match a.val("out") {
        Some(out) => {
            let pth = PathBuf::from(&out);
            if pth.exists() && !a.has("yes") {
                return Err(format!("{} already exists (pass --yes to overwrite)", pth.display()));
            }
            std::fs::write(&pth, &data).map_err(|e| format!("{}: {e}", pth.display()))?;
            println!(
                "{}: {} bytes at {} written to {}",
                util::norm_rel(&path),
                data.len(),
                util::fmt_local(ts),
                pth.display()
            );
        }
        None => {
            let mut out = std::io::stdout();
            out.write_all(&data).map_err(|e| e.to_string())?;
            out.flush().ok();
            if !data.ends_with(b"\n") {
                let _ = writeln!(out);
            }
            eprintln!("({} bytes at {}, sha256 {}…)", data.len(), util::fmt_local(ts), &hash[..12]);
        }
    }
    Ok(EXIT_OK)
}

/// `size` — what each project costs and which files weigh the most.
fn cmd_size(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let top = a.val("top").and_then(|s| s.parse::<usize>().ok()).unwrap_or(10).clamp(1, 200);
    let rows = quick::size_rows(&arch)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &Value::Array(
                    rows.iter()
                        .map(|r| json!({"name": r.name, "bytes": r.bytes, "versions": r.versions, "blobs": r.blobs, "lastObservedAt": r.last_observed.map(iso_ms)}))
                        .collect()
                )
            )
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    let total: u64 = rows.iter().map(|r| r.bytes).sum();
    println!("{:<24} {:>10} {:>9} {:>8}  last observed", "PROJECT", "SIZE", "VERSIONS", "BLOBS");
    for r in &rows {
        println!(
            "{:<24} {:>10} {:>9} {:>8}  {}",
            r.name,
            util::human_size(r.bytes),
            r.versions,
            r.blobs,
            r.last_observed.map(util::fmt_local).unwrap_or_else(|| "-".into())
        );
    }
    println!("{:<24} {:>10}", "TOTAL", util::human_size(total));
    if let Some(space) = quick::space_short(&arch) {
        println!("archive volume: {space}");
    }
    let heaviest = quick::top_blobs(&arch, top)?;
    if !heaviest.is_empty() {
        println!();
        println!("heaviest stored files:");
        for (path, size, refs) in &heaviest {
            println!("  {:>10}  {path}  ({refs} version(s))", util::human_size(*size));
        }
    }
    Ok(EXIT_OK)
}

/// `blame` — every event that touched one path, newest first, with the command to look at each.
fn cmd_blame(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let name = rest.first().ok_or_else(|| "usage: projectlife blame <project> <path>".to_string())?;
    let path = rest.get(1).cloned().ok_or_else(|| "usage: projectlife blame <project> <path>".to_string())?;
    let p = arch.find(name)?;
    let limit = a.val("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).clamp(1, 500);
    let hits = quick::blame(&p, &path, limit)?;
    if a.json {
        let items: Vec<Value> = hits
            .iter()
            .map(|e| {
                json!({
                    "seq": events::seq_of(e),
                    "at": events::ts_of(e),
                    "atIso": iso_ms(events::ts_of(e)),
                    "atLocal": util::fmt_local(events::ts_of(e)),
                    "type": events::event_type(e),
                    "path": util::norm_rel(&path),
                    "size": events::get_u64(e, "size").unwrap_or(0),
                    "hash": events::get_str(e, "hash").unwrap_or_default(),
                    "summary": ev_one_line(e),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"project": p.name, "path": util::norm_rel(&path), "events": items}))
                .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    if hits.is_empty() {
        println!("no event mentions {} in {} — it was never tracked, or filtered out (pl why {} {path})", util::norm_rel(&path), p.name, p.name);
        return Ok(EXIT_OK);
    }
    println!("{}: {} event(s) for {}", p.name, hits.len(), util::norm_rel(&path));
    for e in &hits {
        println!(
            "{:>6}  {}  {:<9} {}",
            events::seq_of(e),
            util::fmt_local(events::ts_of(e)),
            events::event_type(e),
            ev_one_line(e)
        );
        if matches!(events::event_type(e), "put" | "move" | "symlink") {
            println!(
                "        look at it: pl cat {} --path {} --at \"{}\"",
                p.name,
                util::norm_rel(&path),
                util::fmt_local(events::ts_of(e))
            );
        }
    }
    Ok(EXIT_OK)
}

/// `suggest` — one to three actions that follow from the state right now. Prints; never runs.
fn cmd_suggest(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let limit = a.val("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(3).clamp(1, 20);
    let only = rest.first().map(|s| s.as_str());
    if let Some(name) = only {
        arch.find(name)?;
    }
    let all = quick::suggestions(&arch, only)?;
    let rows: Vec<&quick::Suggestion> = all.iter().take(limit).collect();
    if a.json {
        let items: Vec<Value> = rows.iter().map(|s| json!({"project": s.project, "text": s.text, "command": s.command})).collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_default());
        return Ok(EXIT_OK);
    }
    if rows.is_empty() {
        println!("nothing needs attention: the heartbeat is fresh, no gaps, no mass events, no dangling blobs.");
        return Ok(EXIT_OK);
    }
    for s in &rows {
        println!("{}", s.text);
        println!("  -> {}", s.command);
    }
    if all.len() > rows.len() {
        println!("({} more — raise --limit to see them)", all.len() - rows.len());
    }
    Ok(EXIT_OK)
}

/// `prompt` — one line for PS1 or a status bar.
fn cmd_prompt(a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let mut line = quick::prompt_line(&arch);
    if a.has("space") {
        if let Some(s) = quick::space_short(&arch) {
            line.push(' ');
            line.push_str(&s);
        }
    }
    println!("{line}");
    Ok(EXIT_OK)
}

/// `gc --dry-run` — how much of the store nothing references. Deleting is `check --fix`, which
/// asks first; this command exists so the number can be seen without touching anything.
fn cmd_gc(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let projects: Vec<Project> = if rest.is_empty() {
        arch.load_projects()?
    } else {
        vec![arch.find(&rest[0])?]
    };
    let mut total_n = 0usize;
    let mut total_b = 0u64;
    let mut rows: Vec<Value> = Vec::new();
    for p in &projects {
        let (n, bytes, _) = retention::dangling(p)?;
        total_n += n;
        total_b += bytes;
        rows.push(json!({"project": p.name, "dangling": n, "bytes": bytes}));
        if n > 0 {
            println!("{}: {n} unreferenced blob(s), {}", p.name, util::human_size(bytes));
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({"projects": rows, "total": total_n, "bytes": total_b})).unwrap_or_default());
    } else if total_n == 0 {
        println!("nothing to collect: every blob in the store is referenced by the journal");
    } else {
        println!("total: {total_n} unreferenced blob(s), {}", util::human_size(total_b));
        println!("this command never deletes. To remove them:  pl check <project> --fix   (it asks first)");
    }
    Ok(EXIT_OK)
}

/// `notify test` — does the notification channel actually reach you?
fn cmd_notifications(rest: &[String], a: &Args) -> Result<i32, String> {
    let _ = rest;
    let arch = open_archive(a)?;
    let limit = a
        .val("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 1000);
    let kind = a.val("kind");
    let since = match a.val("since") {
        Some(s) => Some(util::parse_at(&s, util::now_ms())?),
        None => None,
    };
    // Read more than asked, then filter, so `--kind` does not silently shorten the list.
    let mut rows = daemon::read_notifications(&arch, if kind.is_some() || since.is_some() { 5000 } else { limit });
    rows.retain(|r| {
        if let Some(k) = &kind {
            if r.get("kind").and_then(|v| v.as_str()) != Some(k.as_str()) {
                return false;
            }
        }
        if let Some(s) = since {
            if r.get("at").and_then(|v| v.as_i64()).unwrap_or(0) < s {
                return false;
            }
        }
        true
    });
    if rows.len() > limit {
        rows.drain(0..rows.len() - limit);
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "archive": arch.root.to_string_lossy(),
                "count": rows.len(),
                "ledger": arch.root.join("logs/notifications.jsonl").to_string_lossy(),
                "notifications": rows,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    if rows.is_empty() {
        println!("no notifications recorded yet ({} )", arch.root.join("logs/notifications.jsonl").display());
        println!("the program writes one line here for every message it raises: a mass change, a stop for");
        println!("space, a restart of writing, an unreachable archive, a cycle error, or `pl notify test`.");
        return Ok(EXIT_OK);
    }
    for r in &rows {
        let local = r.get("local").and_then(|v| v.as_str()).unwrap_or("");
        let k = r.get("kind").and_then(|v| v.as_str()).unwrap_or("notice");
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let project = r.get("project").and_then(|v| v.as_str());
        println!(
            "{local}  {k:<14} {}{}",
            match project {
                Some(p) => format!("[{p}] "),
                None => String::new(),
            },
            title
        );
        if let Some(body) = r.get("body").and_then(|v| v.as_str()) {
            for line in body.lines() {
                println!("                  {line}");
            }
        }
    }
    println!("({} shown; the ledger is {})", rows.len(), arch.root.join("logs/notifications.jsonl").display());
    Ok(EXIT_OK)
}

fn cmd_notify(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let what = rest.first().map(|s| s.as_str()).unwrap_or("test");
    if what != "test" {
        return Err(format!("unknown notify subcommand: {what} (only: test)"));
    }
    daemon::notify_full(&arch, "test", None, "Project Life", "test notification — if you can see this, notifications work");
    println!("notification sent through the configured channel");
    println!("the same line is always written to the archive log, even when no notifier is installed");
    println!("notifications are {} in the configuration (config set notifications false to silence them)", arch.config.bool_of("notifications", true));
    Ok(EXIT_OK)
}

/// `completion <shell> [--install]` — a completion script, generated from the command table.
fn cmd_completion(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let shell = rest.first().map(|s| s.to_lowercase()).unwrap_or_else(|| "bash".to_string());
    let commands = COMMANDS;
    let script = match shell.as_str() {
        "bash" => format!(
            "# projectlife bash completion — source this file\n_pl() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  if [ \"$COMP_CWORD\" -eq 1 ]; then\n    COMPREPLY=( $(compgen -W \"{}\" -- \"$cur\") )\n  fi\n}}\ncomplete -F _pl pl projectlife\n",
            commands.join(" ")
        ),
        "zsh" => format!(
            "#compdef pl projectlife\n_pl() {{\n  local -a cmds\n  cmds=({})\n  _describe 'command' cmds\n}}\n_pl \"$@\"\n",
            commands.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(" ")
        ),
        "fish" => commands
            .iter()
            .map(|c| format!("complete -c pl -n '__fish_use_subcommand' -a {c}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
        other => return Err(format!("unknown shell: {other} (bash, zsh or fish)")),
    };
    if a.has("install") {
        let dir = arch.root.join("completion");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("pl.{shell}"));
        std::fs::write(&file, script.as_bytes()).map_err(|e| e.to_string())?;
        println!("written: {}", file.display());
        match shell.as_str() {
            "bash" => println!("add to ~/.bashrc:  source \"{}\"", file.display()),
            "zsh" => println!("add to ~/.zshrc:   source \"{}\"", file.display()),
            _ => println!("add to ~/.config/fish/config.fish:  source \"{}\"", file.display()),
        }
    } else {
        print!("{script}");
    }
    Ok(EXIT_OK)
}

/// `mcp` — what the agent-facing server offers and how to register it. Prints only.
fn cmd_mcp(rest: &[String], a: &Args) -> Result<i32, String> {
    let arch = open_archive(a)?;
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("info");
    match sub {
        "tools" => {
            for name in mcp::tool_names() {
                println!("{name}");
            }
            Ok(EXIT_OK)
        }
        "info" => {
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "pl-mcp".into());
            let mcp_bin = PathBuf::from(exe).parent().map(|d| d.join("pl-mcp")).unwrap_or_else(|| PathBuf::from("pl-mcp"));
            println!("Project Life MCP server (read-only)");
            println!();
            println!("Registered tools ({}):", mcp::tool_names().len());
            for name in mcp::tool_names() {
                println!("  {name}");
            }
            println!();
            println!("Not registered, and refused if called ({} write operations):", mcp::write_tool_names().len());
            println!("  {}", mcp::write_tool_names().join(", "));
            println!();
            println!("Register it with a client that speaks MCP over stdio:");
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "mcpServers": {
                        "projectlife": {
                            "command": mcp_bin.to_string_lossy(),
                            "args": ["--archive", arch.root.to_string_lossy()]
                        }
                    }
                }))
                .unwrap_or_default()
            );
            println!("The server process itself holds no write path; see docs/ARCHITECTURE.md §MCP.");
            Ok(EXIT_OK)
        }
        other => Err(format!("unknown mcp subcommand: {other} (info|tools)")),
    }
}

/// The command names, in one place: `help` and the completion script are both generated from it.
pub const COMMANDS: &[&str] = &[
    "init-archive", "archive-move", "audit-archive", "add", "detect", "presets", "list", "status", "pause", "resume",
    "remove", "relink", "note", "log", "timeline", "tree", "diff", "restore", "export-file", "mark", "snap", "last-good",
    "panic", "why", "apply-filters", "prune", "retention", "export", "export-and-prune", "import", "archive-delete",
    "check", "verify", "rebuild-cache", "quarantine", "scan-once", "partial-pass", "drill", "daemon", "config", "doctor", "recover",
    "recent", "since", "cat", "size", "blame", "suggest", "undo", "watch", "open", "gc", "notify",
    "notifications", "completion", "prompt",
    "heartbeat-check", "healthcheck", "mcp", "version", "help",
];


/// `list --sort`: one key, no guessing. `risk` is a rank, not a colour: a project whose path is
/// missing or whose journal needs a recovery sorts first.
fn sort_projects(projects: &mut Vec<Project>, key: &str) -> Result<(), String> {
    match key {
        "name" => projects.sort_by(|a, b| a.name.cmp(&b.name)),
        "age" => projects.sort_by_key(|p| {
            p.meta_str("lastObservedAt").and_then(|s| archive::parse_iso_ms(&s)).unwrap_or(0)
        }),
        "size" => {
            let mut with: Vec<(u64, Project)> = projects.drain(..).map(|p| (p.size_on_disk(), p)).collect();
            with.sort_by_key(|(b, _)| std::cmp::Reverse(*b));
            projects.extend(with.into_iter().map(|(_, p)| p));
        }
        "risk" => projects.sort_by_key(|p| std::cmp::Reverse(risk_rank(p))),
        other => return Err(format!("unknown sort key: {other} (name, size, age, risk)")),
    }
    Ok(())
}

fn risk_rank(p: &Project) -> u32 {
    let mut r = 0;
    if p.state() != "active" {
        r += 4;
    }
    if p.prune_journal().is_file() {
        r += 2;
    }
    if p.meta_str("lastGapAt").is_some() {
        r += 1;
    }
    r
}

/// `status --compact`: every project in one table, with the one command that follows from its row.
fn print_status_compact(arch: &Archive, only: Option<&str>) -> Result<i32, String> {
    let rows = quick::status_rows(arch)?;
    let shown: Vec<&quick::StatusRow> = rows.iter().filter(|r| only.map(|o| r.name == o).unwrap_or(true)).collect();
    if shown.is_empty() {
        println!("no projects");
        return Ok(EXIT_OK);
    }
    println!(
        "{:<22} {:<12} {:>8} {:>17} {:>10}  next",
        "PROJECT", "STATE", "VERSIONS", "LAST OBSERVED", "SIZE"
    );
    let mut problems = 0usize;
    for r in &shown {
        if r.state != "active" || r.pending_recovery {
            problems += 1;
        }
        println!(
            "{:<22} {:<12} {:>8} {:>17} {:>10}  {}",
            r.name,
            if r.pending_recovery { "recovery" } else { r.state.as_str() },
            r.versions,
            r.last_observed.map(util::fmt_local).unwrap_or_else(|| "-".into()),
            util::human_size(r.bytes),
            r.next.clone().unwrap_or_else(|| "-".into())
        );
    }
    let hb = health::heartbeat(arch, 60_000);
    println!(
        "{} project(s), {} needing attention, heartbeat {} ({}{})",
        shown.len(),
        problems,
        if hb.fresh { "fresh" } else { "STALE" },
        hb.age_ms.map(|a| format!("{} s ago", a / 1000)).unwrap_or_else(|| "never".into()),
        if hb.daemon.is_some() { ", daemon running" } else { "" }
    );
    Ok(EXIT_OK)
}
