//! Everyday read-only conveniences: what changed, what happened last, who touched a file, what to
//! do next, how big this is.
//!
//! Nothing in this module writes anything — no blob, no journal, no metadata, not even a log line.
//! That is deliberate: the MCP server (Level 1) calls these functions and nothing else, so the
//! short commands and the agent-facing surface are the same code with the same guarantees.

use crate::archive::{iso_ms, Archive, Project};
use crate::events::{self, Ev, FileSt};
use crate::ops;
use crate::restore;
use crate::retention;
use crate::store::Store;
use crate::util;
use std::collections::{BTreeMap, BTreeSet};

fn matches(path: &str, filter: &str) -> bool {
    path == filter || path.starts_with(&format!("{filter}/"))
}

#[derive(Debug, Default)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl Diff {
    pub fn total(&self) -> usize {
        self.added.len() + self.removed.len() + self.changed.len()
    }
}

/// Compare two states. `changed` means "the content differs", never "the timestamp differs":
/// a file that was rewritten with the same bytes is not a change.
pub fn diff_states(a: &BTreeMap<String, FileSt>, b: &BTreeMap<String, FileSt>, filter: Option<&str>) -> Diff {
    let mut d = Diff::default();
    for (k, v) in b.iter() {
        if let Some(f) = filter {
            if !matches(k, f) {
                continue;
            }
        }
        match a.get(k) {
            None => d.added.push(k.clone()),
            Some(old) => {
                let same_hash = !old.hash.is_empty() && old.hash == v.hash;
                let same_link = old.kind == "symlink" && v.kind == "symlink" && old.target == v.target;
                if !same_hash && !same_link {
                    d.changed.push(k.clone());
                }
            }
        }
    }
    for k in a.keys() {
        if let Some(f) = filter {
            if !matches(k, f) {
                continue;
            }
        }
        if !b.contains_key(k) {
            d.removed.push(k.clone());
        }
    }
    d
}

#[derive(Debug, Clone)]
pub struct StatusRow {
    pub name: String,
    pub id: String,
    pub state: String,
    pub profile: String,
    pub versions: usize,
    pub last_observed: Option<i64>,
    pub bytes: u64,
    pub daemon_ok: bool,
    pub last_gap: Option<i64>,
    pub pending_recovery: bool,
    pub stored_policy: Option<String>,
    pub next: Option<String>,
}

impl StatusRow {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name, "id": self.id, "state": self.state, "profile": self.profile,
            "versions": self.versions,
            "lastObservedAt": self.last_observed.map(iso_ms),
            "archiveBytes": self.bytes,
            "daemonRunning": self.daemon_ok,
            "lastGapAt": self.last_gap.map(iso_ms),
            "pendingRecovery": self.pending_recovery,
            "retentionPolicy": self.stored_policy,
            "next": self.next,
        })
    }
}

pub fn status_rows(arch: &Archive) -> Result<Vec<StatusRow>, String> {
    let hb = crate::health::heartbeat(arch, 60_000);
    let mut rows = Vec::new();
    for p in arch.load_projects()? {
        let journal = events::load_journal(&p.dir)?;
        let versions = journal.events.iter().filter(|e| events::event_type(e) == "put").count();
        rows.push(StatusRow {
            name: p.name.clone(),
            id: p.id.clone(),
            state: p.state(),
            profile: p.profile(),
            versions,
            last_observed: events::last_observed_at(&journal.events),
            bytes: p.size_on_disk(),
            daemon_ok: hb.fresh,
            last_gap: p.meta_str("lastGapAt").and_then(|s| crate::archive::parse_iso_ms(&s)),
            pending_recovery: p.prune_journal().is_file(),
            stored_policy: retention::stored_policy(&p),
            next: None,
        });
    }
    let sugg = suggestions(arch, None).unwrap_or_default();
    for row in rows.iter_mut() {
        if let Some(s) = sugg.iter().find(|s| s.project == row.name) {
            row.next = Some(s.command.clone());
        }
    }
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct RecentRow {
    pub project: String,
    pub ts: i64,
    pub kind: String,
    pub detail: String,
    pub command: Option<String>,
}

/// The last few things that matter: mass events, gaps, marks, and what the program itself did.
pub fn recent(arch: &Archive, per_project: usize) -> Result<Vec<RecentRow>, String> {
    let mut rows: Vec<RecentRow> = Vec::new();
    for p in arch.load_projects()? {
        let journal = events::load_journal(&p.dir)?;
        let mut picked: Vec<&Ev> = journal
            .events
            .iter()
            .filter(|e| matches!(events::event_type(e), "mass" | "gap" | "mark"))
            .collect();
        picked.sort_by_key(|e| events::ts_of(e));
        for e in picked.iter().rev().take(per_project) {
            let ts = events::ts_of(e);
            match events::event_type(e) {
                "mass" => rows.push(RecentRow {
                    project: p.name.clone(),
                    ts,
                    kind: format!("mass {}", events::get_str(e, "kind").unwrap_or_default()),
                    detail: format!(
                        "{} files ({} deleted, {} changed)",
                        events::get_u64(e, "files").unwrap_or(0),
                        events::get_u64(e, "deleted").unwrap_or(0),
                        events::get_u64(e, "changed").unwrap_or(0)
                    ),
                    command: Some(format!("pl last-good {}", p.name)),
                }),
                "gap" => {
                    let from = events::get_i64(e, "from").unwrap_or(0);
                    rows.push(RecentRow {
                        project: p.name.clone(),
                        ts,
                        kind: "gap".into(),
                        detail: format!(
                            "{} .. {} ({})",
                            util::fmt_local(from),
                            util::fmt_local(events::get_i64(e, "to").unwrap_or(0)),
                            events::get_str(e, "reason").unwrap_or_default()
                        ),
                        command: Some(format!(
                            "pl restore {} --at \"{}\" --to ../{}-recovered",
                            p.name,
                            util::fmt_local(from.saturating_sub(1000)),
                            p.name
                        )),
                    });
                }
                _ => rows.push(RecentRow {
                    project: p.name.clone(),
                    ts,
                    kind: "mark".into(),
                    detail: events::get_str(e, "label").unwrap_or_default(),
                    command: Some(format!(
                        "pl restore {} --mark \"{}\" --to ../{}-recovered",
                        p.name,
                        events::get_str(e, "label").unwrap_or_default(),
                        p.name
                    )),
                }),
            }
        }
    }
    for v in ops::read_all(arch) {
        let project = v.get("project").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if op.is_empty() {
            continue;
        }
        let detail = match op.as_str() {
            "restore" => format!(
                "restored {} files at {} into {}",
                v.get("restored").and_then(|x| x.as_u64()).unwrap_or(0),
                v.get("atLabel").and_then(|x| x.as_str()).unwrap_or("?"),
                v.get("target").and_then(|x| x.as_str()).unwrap_or("?")
            ),
            "prune" => format!(
                "pruned before {} ({} -> {} versions)",
                v.get("beforeLabel").and_then(|x| x.as_str()).unwrap_or("?"),
                v.get("versionsBefore").and_then(|x| x.as_u64()).unwrap_or(0),
                v.get("versionsAfter").and_then(|x| x.as_u64()).unwrap_or(0)
            ),
            "retention" => format!(
                "applied retention policy \"{}\" ({} versions dropped, {} kept as anchors)",
                v.get("policy").and_then(|x| x.as_str()).unwrap_or("?"),
                v.get("versionsDropped").and_then(|x| x.as_u64()).unwrap_or(0),
                v.get("anchors").and_then(|x| x.as_u64()).unwrap_or(0)
            ),
            "drill" => format!(
                "drill {} in {} ms",
                v.get("result").and_then(|x| x.as_str()).unwrap_or("?"),
                v.get("ms").and_then(|x| x.as_u64()).unwrap_or(0)
            ),
            "panic" => "panic restored the last good state".to_string(),
            "export" => format!("exported to {}", v.get("out").and_then(|x| x.as_str()).unwrap_or("?")),
            "import" => format!("imported from {}", v.get("from").and_then(|x| x.as_str()).unwrap_or("?")),
            _ => op.clone(),
        };
        let command = match op.as_str() {
            "restore" => Some(format!("pl undo {project}")),
            "retention" | "prune" => Some(format!("pl size --top 5")),
            _ => None,
        };
        rows.push(RecentRow { project, ts: ops::ts_of(&v), kind: format!("op:{op}"), detail, command });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.ts));
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub project: String,
    pub text: String,
    pub command: String,
}

/// At most three actions, chosen from what is actually wrong right now, each with the command that
/// does it. This never runs them: it prints them.
pub fn suggestions(arch: &Archive, only: Option<&str>) -> Result<Vec<Suggestion>, String> {
    let mut out: Vec<Suggestion> = Vec::new();
    let hb = crate::health::heartbeat(arch, 60_000);
    if !hb.fresh {
        out.push(Suggestion {
            project: String::new(),
            text: match hb.ts {
                Some(t) => format!("nothing has been observed since {}", util::fmt_local(t)),
                None => "nothing has ever been observed in this archive".into(),
            },
            command: "pl scan-once --all   # or install a timer: pl daemon install --timer 5".into(),
        });
    }
    let space = crate::space::check(arch);
    if space.warn {
        out.push(Suggestion {
            project: String::new(),
            text: space.reason.clone(),
            command: "pl prune <project> --policy \"7d:all,30d:1/day,365d:1/month\" --dry-run".into(),
        });
    }
    for p in arch.load_projects()? {
        if let Some(f) = only {
            if p.name != f {
                continue;
            }
        }
        if p.prune_journal().is_file() {
            out.push(Suggestion {
                project: p.name.clone(),
                text: format!("{}: a prune was interrupted and is waiting", p.name),
                command: format!("pl recover {}", p.name),
            });
            continue;
        }
        if p.state() == "path_missing" {
            out.push(Suggestion {
                project: p.name.clone(),
                text: format!("{}: the project folder is gone, the history is intact", p.name),
                command: format!("pl restore {} --at \"1h ago\" --to ../{}-recovered", p.name, p.name),
            });
            continue;
        }
        let journal = events::load_journal(&p.dir)?;
        let now = util::now_ms();
        if let Some((from, to, why)) = events::gaps(&journal.events).into_iter().last() {
            if now - to < 7 * 86_400_000 {
                out.push(Suggestion {
                    project: p.name.clone(),
                    text: format!("{}: a gap in observation ({why}) between {} and {}", p.name, util::fmt_local(from), util::fmt_local(to)),
                    command: format!("pl restore {} --at \"{}\" --to ../{}-recovered", p.name, util::fmt_local(from.saturating_sub(1000)), p.name),
                });
                continue;
            }
        }
        let mass = journal
            .events
            .iter()
            .filter(|e| events::event_type(e) == "mass")
            .max_by_key(|e| events::ts_of(e))
            .map(|e| (events::ts_of(e), events::get_str(e, "kind").unwrap_or_default()));
        if let Some((ts, kind)) = mass {
            if now - ts < 86_400_000 {
                out.push(Suggestion {
                    project: p.name.clone(),
                    text: format!("{}: a mass event ({kind}) {} ago", p.name, util::fmt_duration(now - ts)),
                    command: format!("pl last-good {}", p.name),
                });
                continue;
            }
        }
        match retention::dangling(&p) {
            Ok((n, bytes, _)) if n > 0 => {
                out.push(Suggestion {
                    project: p.name.clone(),
                    text: format!("{}: {n} blobs in the store are referenced by nothing ({})", p.name, util::human_size(bytes)),
                    command: "pl gc --dry-run".into(),
                });
                continue;
            }
            _ => {}
        }
        if let Some(policy) = retention::stored_policy(&p) {
            let applied = retention::applied_at(&p).and_then(|s| crate::archive::parse_iso_ms(&s));
            if applied.map(|t| now - t > 30 * 86_400_000).unwrap_or(true) {
                out.push(Suggestion {
                    project: p.name.clone(),
                    text: match applied {
                        Some(t) => format!("{}: the stored retention policy was last applied {}", p.name, util::fmt_local(t)),
                        None => format!("{}: a retention policy is stored but has never been applied", p.name),
                    },
                    command: format!("pl prune {} --policy \"{policy}\" --dry-run", p.name),
                });
                continue;
            }
        }
        let drilled = p.settings().get("lastDrillAt").and_then(|v| v.as_str()).and_then(crate::archive::parse_iso_ms);
        let history_start = p.history_starts_at();
        if drilled.map(|t| now - t > 30 * 86_400_000).unwrap_or(true) && now - history_start > 86_400_000 {
            out.push(Suggestion {
                project: p.name.clone(),
                text: match drilled {
                    Some(t) => format!("{}: the last rehearsal of a real loss was {}", p.name, util::fmt_local(t)),
                    None => format!("{}: the restore promise has never been rehearsed on this machine", p.name),
                },
                command: format!("pl drill {}", p.name),
            });
            continue;
        }
        let unreachable = journal
            .events
            .iter()
            .filter(|e| events::event_type(e) == "skip" && events::get_str(e, "reason").as_deref() == Some("unreadable"))
            .count();
        if unreachable > 0 {
            out.push(Suggestion {
                project: p.name.clone(),
                text: format!("{}: {unreachable} files could not be read (permissions?)", p.name),
                command: format!("pl status {} --skipped", p.name),
            });
        }
    }
    Ok(out)
}

/// Every event that touched one path, newest first.
pub fn blame(project: &Project, path: &str, limit: usize) -> Result<Vec<Ev>, String> {
    let journal = events::load_journal(&project.dir)?;
    let want = util::norm_rel(path);
    let mut hits: Vec<Ev> = journal
        .events
        .iter()
        .filter(|e| {
            [events::get_str(e, "path"), events::get_str(e, "to"), events::get_str(e, "from")]
                .into_iter()
                .flatten()
                .any(|x| matches(&x, &want))
        })
        .cloned()
        .collect();
    hits.sort_by_key(events::seq_of);
    hits.reverse();
    hits.truncate(limit);
    Ok(hits)
}

/// The bytes of one file at one moment, read through the verified store. Read-only.
pub fn cat_version(project: &Project, path: &str, at: i64) -> Result<(Vec<u8>, i64, String), String> {
    let journal = events::load_journal(&project.dir)?;
    let hstart = project.history_starts_at();
    if hstart > 0 && at < hstart {
        return Err(format!(
            "{} is earlier than the start of the available history ({})",
            util::fmt_local(at),
            util::fmt_local(hstart)
        ));
    }
    let want = util::norm_rel(path);
    let state = events::state_at(&journal.events, at, None);
    let f = state.get(&want).ok_or_else(|| {
        format!("{want} is not in the state at {} (not tracked then, or not in this project)", util::fmt_local(at))
    })?;
    if f.kind == "symlink" {
        return Err(format!("{want} was a symlink to {} at that moment", f.target.clone().unwrap_or_default()));
    }
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let data = store.read_verified(&f.hash)?;
    Ok((data, f.ts, f.hash.clone()))
}

/// A moment in this project where the whole project state is available: used by `cat` and the API.
pub fn last_observed(project: &Project) -> Result<Option<i64>, String> {
    Ok(events::last_observed_at(&events::load_journal(&project.dir)?.events))
}

pub fn last_good_of(project: &Project) -> Result<Option<(i64, String)>, String> {
    let journal = events::load_journal(&project.dir)?;
    Ok(restore::last_good(&journal.events, project))
}

#[derive(Debug, Clone)]
pub struct SizeRow {
    pub name: String,
    pub bytes: u64,
    pub versions: usize,
    pub blobs: usize,
    pub last_observed: Option<i64>,
}

pub fn size_rows(arch: &Archive) -> Result<Vec<SizeRow>, String> {
    let mut out = Vec::new();
    for p in arch.load_projects()? {
        let journal = events::load_journal(&p.dir).unwrap_or(events::Journal { events: Vec::new(), trailing_partial: false });
        let store = Store::new(&p.blobs_dir(), &p.tmp_dir());
        out.push(SizeRow {
            name: p.name.clone(),
            bytes: p.size_on_disk(),
            versions: journal.events.iter().filter(|e| events::event_type(e) == "put").count(),
            blobs: store.list_all().len(),
            last_observed: events::last_observed_at(&journal.events),
        });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
    Ok(out)
}

/// The heaviest stored files across the archive, with the paths that use them.
pub fn top_blobs(arch: &Archive, n: usize) -> Result<Vec<(String, u64, usize)>, String> {
    let mut by_hash: BTreeMap<String, (u64, BTreeSet<String>)> = BTreeMap::new();
    for p in arch.load_projects()? {
        let journal = match events::load_journal(&p.dir) {
            Ok(j) => j,
            Err(_) => continue,
        };
        for e in &journal.events {
            if events::event_type(e) != "put" {
                continue;
            }
            if let (Some(h), Some(path)) = (events::get_str(e, "hash"), events::get_str(e, "path")) {
                let slot = by_hash.entry(h).or_insert((events::get_u64(e, "size").unwrap_or(0), BTreeSet::new()));
                slot.1.insert(format!("{}:{}", p.name, path));
            }
        }
    }
    let mut rows: Vec<(String, u64, usize)> = by_hash
        .into_iter()
        .map(|(_, (size, paths))| (paths.iter().next().cloned().unwrap_or_default(), size, paths.len()))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    rows.truncate(n);
    Ok(rows)
}

/// One line for a shell prompt. Never a command, never a write.
pub fn prompt_line(arch: &Archive) -> String {
    let hb = crate::health::heartbeat(arch, 60_000);
    let projects = arch.load_projects().map(|p| p.len()).unwrap_or(0);
    let flag = if hb.fresh { "ok" } else { "STALE" };
    let age = match hb.age_ms {
        Some(a) => format!("{}s", a / 1000),
        None => "never".into(),
    };
    format!("pl:{projects}p {flag} {age}")
}

/// The archive's own free space against the configured thresholds, for the prompt and `suggest`.
pub fn space_short(arch: &Archive) -> Option<String> {
    let free = arch.free_bytes()?;
    let total = arch.total_bytes().unwrap_or(free).max(1);
    let pct = (free as f64) * 100.0 / total as f64;
    Some(format!("{} free ({pct:.0} %)", util::human_size(free)))
}
