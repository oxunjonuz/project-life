//! `doctor` and shared archive checks: OK / WARN / ERROR plus the command that fixes each problem.

use crate::archive::{Archive, Project};
use crate::events::{self, get_str};
use crate::store::Store;
use crate::util;
use std::collections::BTreeSet;

pub const OK: &str = "OK";
pub const WARN: &str = "WARN";
pub const ERROR: &str = "ERROR";

pub struct Check {
    pub level: &'static str,
    pub text: String,
    pub fix: Option<String>,
}

/// The remedy that can work while the archive is full. Handing out a command that cannot write
/// (`pl scan-once`, `pl daemon start`) in this state is how the owner's Mac came to be told to
/// start a daemon that was already running.
fn space_remedy(archive: &Archive) -> String {
    format!(
        "free space on the volume holding {} — nothing is lost while writing is stopped: a pass that cannot store \
         does not advance the state, so the next one records the change",
        archive.root.display()
    )
}

pub fn doctor(archive: &Archive) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(Check { level: OK, text: format!("archive reachable: {}", archive.root.display()), fix: None });

    let space = crate::space::check(archive);
    match space.free {
        Some(_) => {
            // The sentence, the numbers and which of the two rules won — all from `space`, so the
            // doctor cannot report a different verdict than the daemon obeys.
            let level = if space.stop {
                ERROR
            } else if space.warn {
                WARN
            } else {
                OK
            };
            out.push(Check {
                level,
                text: space.reason.clone(),
                // A remedy that cannot work is worse than none: while nothing is being written
                // (2026-10-06), this line offered `pl prune … --dry-run`, which deletes nothing and
                // therefore frees nothing. The dry run shows the plan; only the real command frees.
                fix: if space.stop {
                    Some(
                        "write is stopped until there is room: free space on that volume, or drop versions with \
                         `pl prune <project> --policy \"7d:all,30d:1/day,365d:1/month\"` (without --dry-run it performs \
                         the plan; --dry-run only prints it)"
                            .into(),
                    )
                } else if space.warn {
                    Some(
                        "pl prune <project> --policy \"7d:all,30d:1/day,365d:1/month\" --dry-run   # see what could be \
                         dropped; the real command frees the space"
                            .into(),
                    )
                } else {
                    None
                },
            });
        }
        None => out.push(Check {
            level: ERROR,
            text: space.reason.clone(),
            fix: Some("check that the archive disk is connected".into()),
        }),
    }

    match archive.heartbeat_ms() {
        Some(ts) => {
            let age = util::now_ms() - ts;
            if age > 60_000 {
                if space.stop {
                    // The daemon may be alive and cycling; what is missing is room, and no advice
                    // to start it or to run a pass can help. (2026-10-06: this line said "daemon not
                    // running" about a daemon that was running.)
                    out.push(Check {
                        level: WARN,
                        text: format!(
                            "no cycle can finish: the last attempt was {} and writing is stopped — {}",
                            util::fmt_local(ts),
                            space.reason
                        ),
                        fix: Some(space_remedy(archive)),
                    });
                } else {
                    out.push(Check {
                        level: WARN,
                        text: format!("daemon not running: last cycle {}", util::fmt_local(ts)),
                        fix: Some("pl daemon start   # or: pl scan-once --all from a scheduler".into()),
                    });
                }
            } else {
                out.push(Check {
                    level: OK,
                    text: format!("last cycle {} ({} s ago)", util::fmt_local(ts), age / 1000),
                    fix: None,
                });
            }
        }
        None if space.stop => out.push(Check {
            level: WARN,
            text: "no heartbeat: writing is stopped, so no pass could have completed — the archive is full".into(),
            fix: Some(space_remedy(archive)),
        }),
        None => out.push(Check {
            level: WARN,
            text: "no heartbeat: neither the daemon nor scan-once has run yet".into(),
            fix: Some("pl scan-once --all".into()),
        }),
    }

    if archive.root.join(".lock").exists() {
        let raw = std::fs::read_to_string(archive.root.join(".lock")).unwrap_or_default();
        let info = crate::archive::parse_lock(&raw);
        let alive = info.pid.map(crate::archive::pid_alive);
        match alive {
            Some(true) => out.push(Check {
                level: WARN,
                text: format!("lock file present, held by a RUNNING process ({}): a writer cycle is in progress", info.raw),
                fix: None,
            }),
            Some(false) => out.push(Check {
                level: WARN,
                text: format!(
                    "lock file present, but the process that wrote it (pid {}) is gone — the next cycle takes it over; to remove it now:",
                    info.pid.unwrap_or(0)
                ),
                fix: Some("pl doctor --fix-lock".into()),
            }),
            None => out.push(Check {
                level: WARN,
                text: format!("lock file present ({}): written by a build that recorded no pid", info.raw),
                fix: Some("pl doctor --fix-lock".into()),
            }),
        }
    }

    // The daemon-lifetime file (`.daemon`): a second daemon start is refused while a live holder
    // exists, and a file left behind by a killed daemon is taken over by the next start.
    if archive.root.join(".daemon").exists() {
        let raw = std::fs::read_to_string(archive.root.join(".daemon")).unwrap_or_default();
        let info = crate::archive::parse_lock(&raw);
        let alive = info.pid.map(crate::archive::pid_alive);
        match alive {
            Some(true) => out.push(Check {
                level: OK,
                text: format!("daemon lock held by a running daemon ({}) — a second start will be refused", info.raw),
                fix: None,
            }),
            Some(false) => out.push(Check {
                level: WARN,
                text: format!(
                    "daemon lock present, but the daemon that wrote it (pid {}) is gone — the next `daemon run` takes it over",
                    info.pid.unwrap_or(0)
                ),
                fix: Some("pl doctor --fix-lock".into()),
            }),
            None => out.push(Check {
                level: WARN,
                text: format!("daemon lock present ({}): written by a build that recorded no pid", info.raw),
                fix: Some("pl doctor --fix-lock".into()),
            }),
        }
    }

    let projects = match archive.load_projects() {
        Ok(p) => p,
        Err(e) => {
            out.push(Check { level: ERROR, text: format!("projects are unreadable: {e}"), fix: None });
            return out;
        }
    };
    out.push(Check { level: OK, text: format!("projects in the archive: {}", projects.len()), fix: None });
    for p in &projects {
        checks_for_project(archive, p, &mut out);
    }
    out
}

pub fn checks_for_project(archive: &Archive, project: &Project, out: &mut Vec<Check>) {
    let name = project.name.clone();
    let root = project.project_path();
    if !root.is_dir() {
        out.push(Check {
            level: ERROR,
            text: format!("{name}: project folder unreachable ({})", root.display()),
            fix: Some(format!("pl relink {name} <new-path>")),
        });
    }
    match project.state().as_str() {
        "error" => out.push(Check {
            level: ERROR,
            text: format!("{name}: project is in state error (journal corrupted in the middle)"),
            fix: Some(format!("pl check {name} --deep; python3 recover.py check --project {name}")),
        }),
        "paused" => out.push(Check { level: OK, text: format!("{name}: observation paused"), fix: None }),
        "path_missing" => out.push(Check {
            level: WARN,
            text: format!("{name}: the project path disappeared"),
            fix: Some(format!("pl restore {name} --at \"1h ago\" --to <dir>")),
        }),
        _ => {}
    }
    if project.prune_journal().is_file() {
        out.push(Check {
            level: WARN,
            text: format!(
                "{name}: a prune was interrupted (prune.journal is present) — the next cycle or `recover` finishes or rolls it back"
            ),
            fix: Some(format!("pl recover {name}")),
        });
    }
    let journal = match events::load_journal(&project.dir) {
        Ok(j) => j,
        Err(e) => {
            out.push(Check { level: ERROR, text: format!("{name}: {e}"), fix: Some("python3 recover.py check".into()) });
            return;
        }
    };
    if journal.trailing_partial {
        out.push(Check {
            level: WARN,
            text: format!("{name}: the last journal line is half-written (after a crash) — ignored, history intact"),
            fix: None,
        });
    }
    if journal.events.is_empty() {
        out.push(Check {
            level: WARN,
            text: format!("{name}: journal is empty (no observations yet)"),
            fix: Some(format!("pl scan-once {name}")),
        });
    }
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let referenced: BTreeSet<String> = journal
        .events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .filter_map(|e| get_str(e, "hash"))
        .collect();
    let missing: Vec<String> = referenced.iter().filter(|h| !store.has(h)).cloned().collect();
    if !missing.is_empty() {
        out.push(Check {
            level: ERROR,
            text: format!(
                "{name}: {} events without a blob (e.g. {})",
                missing.len(),
                &missing[0][..12.min(missing[0].len())]
            ),
            fix: Some(format!("pl check {name} --deep")),
        });
    } else {
        out.push(Check {
            level: OK,
            text: format!("{name}: journal references are intact ({} versions)", referenced.len()),
            fix: None,
        });
    }
    let all = store.list_all();
    let dangling: Vec<String> = all.iter().map(|(h, _)| h.clone()).filter(|h| !referenced.contains(h)).collect();
    if !dangling.is_empty() {
        out.push(Check {
            level: WARN,
            text: format!("{name}: dangling blobs: {} (safe to delete)", dangling.len()),
            fix: Some(format!("pl check {name} --fix --yes")),
        });
    }
    let initial = journal.events.iter().any(|e| events::event_type(e) == "snapshot");
    if !initial {
        out.push(Check {
            level: WARN,
            text: format!("{name}: no initial snapshot (was the project added with --no-initial?)"),
            fix: Some(format!("pl scan-once {name}")),
        });
    }
    if root.is_dir() && same_volume(&root, &archive.root) {
        out.push(Check {
            level: WARN,
            text: format!("{name}: archive and project share one volume — losing the volume loses both"),
            fix: Some("move the archive to another disk: pl archive-move <new-path>".into()),
        });
    }
    let secrets_in_use = archive.config.bool_of("includeSecrets", false)
        || project.settings().get("includeSecrets").and_then(|v| v.as_bool()).unwrap_or(false);
    if secrets_in_use {
        out.push(Check {
            level: WARN,
            text: format!("{name}: secrets inclusion is on and the archive is not encrypted"),
            fix: Some("turn includeSecrets off unless you really need it".into()),
        });
    }
    let size = project.size_on_disk();
    if size > 10 * 1024 * 1024 * 1024 {
        out.push(Check { level: WARN, text: format!("{name}: project archive is {}", util::human_size(size)), fix: None });
    }
}

pub fn same_volume(a: &std::path::Path, b: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Archive digest: sha256 of every archive file. Detects substitution and deletion inside the archive.
pub fn archive_digest(archive: &Archive, update: bool) -> Result<(usize, Vec<String>), String> {
    let mut rows: Vec<(String, String, u64)> = Vec::new();
    let mut stack = vec![archive.root.clone()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if name == "manifest" || name == ".lock" || name == "heartbeat" {
                continue;
            }
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(&archive.root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            let data = std::fs::read(&p).map_err(|e| format!("{rel}: {e}"))?;
            rows.push((rel, crate::store::sha256_bytes(&data), data.len() as u64));
        }
    }
    rows.sort();
    let dir = archive.root.join("manifest");
    let mut prev: Option<String> = None;
    if dir.is_dir() {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        if let Some(f) = files.last() {
            prev = std::fs::read_to_string(f).ok();
        }
    }
    let mut problems = Vec::new();
    if let Some(text) = &prev {
        let mut known: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let (Some(p), Some(h)) = (
                    v.get("path").and_then(|x| x.as_str()),
                    v.get("hash").and_then(|x| x.as_str()),
                ) {
                    known.insert(p.to_string(), h.to_string());
                }
            }
        }
        let current: std::collections::BTreeMap<String, String> =
            rows.iter().map(|(p, h, _)| (p.clone(), h.clone())).collect();
        for (p, h) in &known {
            match current.get(p) {
                None => problems.push(format!("REMOVED from the archive: {p}")),
                Some(cur) if cur != h => problems.push(format!("CHANGED inside the archive: {p}")),
                _ => {}
            }
        }
    }
    if update {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (y, m, d) = util::local_day(util::now_ms());
        let path = dir.join(format!("{y:04}-{m:02}-{d:02}.jsonl"));
        let mut text = String::new();
        let now = util::now_ms();
        for (p, h, sz) in &rows {
            text.push_str(&serde_json::json!({"path": p, "hash": h, "size": sz, "ts": now}).to_string());
            text.push('\n');
        }
        util::write_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok((rows.len(), problems))
}
