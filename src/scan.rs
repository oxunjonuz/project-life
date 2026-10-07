//! Observation of a project: walk with filters, compare `(mtime, size)`, read and hash, write blobs
//! and events, detect `move`, raise mass events, maintain the state cache.
//!
//! The order inside a cycle is strict: contents first (blob), events only after that. A cycle never
//! modifies the project — it only reads.

use crate::archive::{iso_ms, Archive, Project};
use crate::cache::{self, CacheEntry, CacheStore};
use crate::events::{self, Ev};
use crate::filters::{Decision, FilterConfig, R_UNREADABLE};
use crate::glob::IgnoreRules;
use crate::store::{self, Store};
use crate::util;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The whole tracked state, as a map. The pass itself uses `CacheStore` (which reads only what it
/// touched); this is the materialised view — `pl apply-filters`, `pl rebuild-cache`, the tests, and
/// any caller that genuinely wants every entry.
pub struct StateCache {
    pub files: BTreeMap<String, CacheEntry>,
}

impl StateCache {
    pub fn load(project: &Project, events: &[Ev]) -> StateCache {
        match cache::materialize(project) {
            Ok(m) => StateCache { files: m },
            Err(_) => Self::from_journal(events),
        }
    }

    /// Rebuild the cache from the journal — the cache can always be deleted.
    pub fn from_journal(events: &[Ev]) -> StateCache {
        StateCache { files: from_journal_map(events) }
    }

    pub fn save(&self, project: &Project) -> Result<(), String> {
        cache::write_snapshot(project, &self.files, "rebuild-cache")
    }
}

/// The state the journal implies, as cache entries (`dev`/`ino` are not in the journal: they are
/// added by the pass that sees the files, and their absence only costs a move detection).
pub fn from_journal_map(events: &[Ev]) -> BTreeMap<String, CacheEntry> {
    let st = events::state_at(events, i64::MAX, None);
    let mut files = BTreeMap::new();
    for (path, f) in st {
        files.insert(
            path,
            CacheEntry {
                hash: f.hash,
                size: f.size,
                mtime: f.ts,
                mode: f.mode,
                kind: f.kind,
                target: f.target,
                dev: None,
                ino: None,
            },
        );
    }
    files
}

pub struct CurFile {
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    /// "file" or "symlink" — a symlink is recorded, never dereferenced.
    pub kind: String,
}

pub struct ScanOptions {
    /// `initial`, `observed`, `startup`, `pre_restore`, `filters_changed`, `manual`, `deep_verify`
    pub reason: String,
    /// Deep verification: re-hash every file regardless of mtime.
    pub deep: bool,
    /// Print what started and stopped being tracked (after a filter change).
    pub verbose_filters: bool,
    /// Count only — used for the estimate before `add`.
    pub dry_run: bool,
    pub with_initial_snapshot: bool,
    /// Print the skip list with file counts (`scan-once --skipped`). Off by default: counting a
    /// skipped directory means traversing it, which the ordinary cycle must not do.
    pub count_skipped: bool,
    /// Round 293: `Some(scope)` makes this a **partial pass** — only the paths the notifications
    /// named are walked, and only the tracked paths those notifications cover may be written as
    /// new, changed or deleted. `None` is the ordinary full pass. The scope restricts the walk; it
    /// is not a second code path — same loop, same order, same cache, same journal.
    pub scope: Option<Scope>,
}

/// The set of paths a partial pass is allowed to touch (round 293).
///
/// It is built from the notification paths alone. The rule that keeps a partial pass honest is
/// `covers()`: a tracked path may be written as changed or deleted **only** if a notification named
/// it, its parent directory, or an ancestor directory of it. Everything else is carried over from
/// the state cache untouched, so a lost notification costs latency and never costs a version — the
/// periodic full pass is what bounds the promise, and this pass only narrows the work.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Notification paths that are (or were) directories: their whole subtree is walked. A path that
    /// no longer exists stays here — nothing is walked, but everything recorded under it counts as
    /// covered, which is how a deleted file or a deleted folder becomes a `delete` event.
    pub dirs: BTreeSet<String>,
    /// Notification paths that are files or symlinks: exactly these may be read and stored.
    pub files: BTreeSet<String>,
    /// A notification about the project root itself. The whole project is the scope, so the pass is
    /// an ordinary full pass (only the reason and the log line say why it ran).
    pub full: bool,
    /// How many notification paths were handed in (for the report and the log).
    pub paths_in: usize,
}

impl Scope {
    /// Classify notification paths into "walk this subtree" and "look at this file".
    ///
    /// The filesystem is asked, not guessed: a path that is a directory is one, a path that vanished
    /// is kept as a directory (a vanished file and a vanished directory both mean "everything
    /// recorded here is gone"), and no filters are applied here — the walk applies them, so the
    /// reasons and rules a skip carries are the ones the ordinary pass would produce.
    pub fn from_paths(project_root: &Path, paths: &[String]) -> Scope {
        let mut sc = Scope { paths_in: paths.len(), ..Default::default() };
        for raw in paths {
            let rel = normalize_rel(raw);
            if rel.is_empty() {
                sc.full = true;
                continue;
            }
            // A path with `..` or an absolute prefix would leave the project; the walk refuses
            // those, and so does this: it is not a notification about anything we observe.
            if rel.starts_with('/') || rel.split('/').any(|c| c == "..") {
                continue;
            }
            match fs::symlink_metadata(project_root.join(&rel)) {
                Ok(md) if md.is_dir() => {
                    sc.dirs.insert(rel);
                }
                Ok(_) => {
                    sc.files.insert(rel);
                }
                Err(_) => {
                    // Vanished (or unreadable): nothing to walk, but it is what the notification is
                    // about, so whatever the journal holds at or under it is in scope.
                    sc.dirs.insert(rel);
                }
            }
        }
        sc
    }

    /// May this tracked path be written as changed or deleted by this pass?
    pub fn covers(&self, rel: &str) -> bool {
        if self.full || self.files.contains(rel) || self.dirs.contains(rel) {
            return true;
        }
        let mut cur = rel;
        while let Some(i) = cur.rfind('/') {
            cur = &cur[..i];
            if self.dirs.contains(cur) {
                return true;
            }
        }
        false
    }

    pub fn describe(&self) -> String {
        if self.full {
            return format!("{} notification path(s), the project root was named — full pass", self.paths_in);
        }
        format!(
            "{} notification path(s): {} directory subtree(s), {} file(s)",
            self.paths_in,
            self.dirs.len(),
            self.files.len()
        )
    }
}

/// `./src//a.txt/` -> `src/a.txt`. Kept separate from the walk so a notification path can be
/// compared to a recorded path as text.
pub fn normalize_rel(raw: &str) -> String {
    let mut s = raw.replace('\\', "/");
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    while let Some(rest) = s.strip_prefix('/') {
        s = rest.to_string();
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    while s.ends_with('/') {
        s.pop();
    }
    s
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            reason: "observed".into(),
            deep: false,
            verbose_filters: false,
            dry_run: false,
            with_initial_snapshot: false,
            count_skipped: false,
            scope: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub project: String,
    pub files_on_disk: usize,
    pub tracked_before: usize,
    pub created: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub moved: usize,
    pub skipped: usize,
    pub bytes_read: u64,
    pub blobs_new: usize,
    pub mass: Option<Ev>,
    pub skipped_by_reason: BTreeMap<String, usize>,
    pub skipped_paths: Vec<(String, String, String)>,
    pub errors: Vec<String>,
    pub unstable: usize,
    pub duration_ms: i64,
    /// Round 293: true when this pass was restricted to the notification paths. `files_on_disk` is
    /// then the number of files *in scope*, not the number of files in the project — which is what
    /// makes a partial pass's cost measurable rather than assumed.
    pub partial: bool,
    pub scope_paths: usize,
    pub dirs_walked: usize,
    /// Round 294 — the bookkeeping, as numbers. `cache_bytes_read` is what this pass read out of the
    /// cache files; `journal_full_bytes_read` is what it read by reading journal files whole (a
    /// partial pass must leave that at zero); `cache_base_rewrites` is how many times it wrote the
    /// whole base (a partial pass must leave that at zero too); `cache_delta_records` is what it
    /// appended instead.
    pub cache_bytes_read: u64,
    pub journal_full_bytes_read: u64,
    pub journal_tail_bytes_read: u64,
    pub skip_map_bytes_read: u64,
    pub cache_base_rewrites: usize,
    pub cache_delta_records: usize,
    pub cache_delta_records_before: usize,
    pub cache_compactions: usize,
    pub cache_rebuilt: bool,
}

pub fn open_ignore_rules(project_root: &Path) -> (IgnoreRules, IgnoreRules) {
    let own = fs::read_to_string(project_root.join(".projectlifeignore"))
        .map(|t| IgnoreRules::parse(&t))
        .unwrap_or_else(|_| IgnoreRules::empty());
    let git = fs::read_to_string(project_root.join(".gitignore"))
        .map(|t| IgnoreRules::parse(&t))
        .unwrap_or_else(|_| IgnoreRules::empty());
    (own, git)
}

fn mdts(md: &fs::Metadata) -> i64 {
    md.modified().map(util::ms_of).unwrap_or(0)
}

/// Walk a project: tracked files plus every skip with its reason and rule.
///
/// `count_skipped` decides whether a skipped directory is *sized* (its files counted). Counting is a
/// second full traversal of that directory, so it is off in the ordinary observation cycle
/// (NFR-PRF-2) and on only where a number is actually printed: the estimate before `add` and
/// `scan-once --skipped`.
pub fn walk(
    project_root: &Path,
    filter: &FilterConfig,
    archive_prefixes: &[PathBuf],
    ignore: &IgnoreRules,
    gitignore: &IgnoreRules,
    count_skipped: bool,
) -> (BTreeMap<String, CurFile>, Vec<(String, String, String)>, Vec<String>) {
    let (files, skips, errors, _dirs, _read) = walk_from(
        project_root,
        vec![(project_root.to_path_buf(), String::new())],
        filter,
        archive_prefixes,
        ignore,
        gitignore,
        count_skipped,
    );
    (files, skips, errors)
}

/// The walk itself, from an explicit list of starting directories.
///
/// A full pass starts at `(project_root, "")`; a partial pass starts at every directory a
/// notification named. Everything else is identical — the same filters, the same skips with their
/// reasons and rules, the same symlink rule — so a partial pass cannot see the tree differently,
/// only less of it. The returned `usize` is how many directories were actually opened, which is the
/// quantity a partial pass's cost is proportional to.
pub fn walk_from(
    _project_root: &Path,
    starts: Vec<(PathBuf, String)>,
    filter: &FilterConfig,
    archive_prefixes: &[PathBuf],
    ignore: &IgnoreRules,
    gitignore: &IgnoreRules,
    count_skipped: bool,
) -> (BTreeMap<String, CurFile>, Vec<(String, String, String)>, Vec<String>, usize, BTreeSet<String>) {
    let mut files = BTreeMap::new();
    let mut skips: Vec<(String, String, String)> = Vec::new();
    let mut errors = Vec::new();
    let mut dirs_walked = 0usize;
    // Every directory whose listing was actually performed. A partial pass uses this to decide
    // whether it is allowed to call a missing path *deleted*: a path whose parent was never listed
    // may simply be outside the scope, and "I did not look" must never be written as "it is gone".
    let mut dirs_read: BTreeSet<String> = BTreeSet::new();
    // The archive root is canonicalised before the comparison: a prefix that is itself reached
    // through a symlink (e.g. /tmp on macOS) would otherwise never match `canonicalize()` output,
    // and a symlink into the archive would be recorded instead of skipped.
    let ap: Vec<String> = archive_prefixes
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()).to_string_lossy().replace('\\', "/"))
        .collect();
    let mut stack: Vec<(PathBuf, String)> = starts;
    while let Some((dir, rel)) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{rel}: {e}"));
                continue;
            }
        };
        dirs_walked += 1;
        dirs_read.insert(rel.clone());
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let path = e.path();
            let md = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(err) => {
                    skips.push((child_rel.clone(), R_UNREADABLE.into(), err.to_string()));
                    continue;
                }
            };
            if md.file_type().is_symlink() {
                // Symlinks are never dereferenced.
                let target = fs::read_link(&path)
                    .map(|t| t.to_string_lossy().to_string())
                    .unwrap_or_default();
                if let Ok(a) = path.canonicalize() {
                    let astr = a.to_string_lossy().replace('\\', "/");
                    if ap.iter().any(|pr| !pr.is_empty() && astr.starts_with(pr.as_str())) {
                        skips.push((child_rel, "symlink_to_archive".into(), target));
                        continue;
                    }
                }
                files.insert(
                    child_rel,
                    CurFile {
                        size: 0,
                        mtime: mdts(&md),
                        mode: util::file_mode(&md),
                        dev: None,
                        ino: None,
                        kind: "symlink".into(),
                    },
                );
                continue;
            }
            if md.is_dir() {
                let d = filter.decide_dir(&child_rel, &ap, ignore, gitignore);
                if d.track {
                    stack.push((path, child_rel));
                } else if count_skipped {
                    let n = count_files_below(&path);
                    skips.push((
                        child_rel,
                        d.reason.unwrap_or_default(),
                        format!("{} ({} files)", d.rule.unwrap_or_default(), n),
                    ));
                } else {
                    // No counting: the skipped directory is named, never opened. One traversal of
                    // node_modules per cycle is what NFR-PRF-2 forbids.
                    skips.push((child_rel, d.reason.unwrap_or_default(), d.rule.unwrap_or_default()));
                }
                continue;
            }
            if !md.is_file() {
                continue;
            }
            let size = md.len();
            let decision: Decision = filter.decide_file(&child_rel, size, ignore, gitignore);
            if !decision.track {
                skips.push((
                    child_rel,
                    decision.reason.unwrap_or_default(),
                    decision.rule.unwrap_or_default(),
                ));
                continue;
            }
            let (dev, ino) = match util::file_id(&md) {
                Some((d, i)) => (Some(d), Some(i)),
                None => (None, None),
            };
            files.insert(
                child_rel,
                CurFile { size, mtime: mdts(&md), mode: util::file_mode(&md), dev, ino, kind: "file".into() },
            );
        }
    }
    (files, skips, errors, dirs_walked, dirs_read)
}

/// What a scoped walk produced, including what it *looked at* — the two sets a partial pass needs
/// before it may write a deletion (`dirs_read`) and before it may treat a whole subtree as gone
/// (`vanished`).
pub struct ScopedWalk {
    pub files: BTreeMap<String, CurFile>,
    pub skips: Vec<(String, String, String)>,
    pub errors: Vec<String>,
    /// How many directories were opened.
    pub dirs_walked: usize,
    /// Directories whose listing this pass really performed.
    pub dirs_read: BTreeSet<String>,
    /// Scope directories that no longer exist (or are no longer directories). Everything recorded at
    /// or under one of these is genuinely gone.
    pub vanished: BTreeSet<String>,
}

/// The walk a partial pass performs: the notified directory subtrees, plus the notified files.
///
/// Deliberately *not* included: the rest of the project. A notification about a file does not make
/// its directory a scope — listing a directory to look for deleted entries is a different thing
/// from deciding to read and store every file in it, and only the former happens here (the deletion
/// side is decided from the cache by `Scope::covers`, not by walking).
pub fn walk_scoped(
    project_root: &Path,
    filter: &FilterConfig,
    archive_prefixes: &[PathBuf],
    ignore: &IgnoreRules,
    gitignore: &IgnoreRules,
    count_skipped: bool,
    scope: &Scope,
) -> ScopedWalk {
    let ap: Vec<String> = archive_prefixes
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()).to_string_lossy().replace('\\', "/"))
        .collect();
    let mut skips: Vec<(String, String, String)> = Vec::new();
    let mut starts: Vec<(PathBuf, String)> = Vec::new();
    let mut vanished: BTreeSet<String> = BTreeSet::new();
    let mut dirs: Vec<String> = scope.dirs.iter().cloned().collect();
    dirs.sort();
    for d in dirs {
        let path = project_root.join(if d.is_empty() { "." } else { d.as_str() });
        if !path.is_dir() {
            // A notification about a path that is gone (or is not a directory any more): nothing to
            // walk — but everything recorded at or under it is gone too, and that is what the
            // caller needs to know before it calls any of it deleted.
            vanished.insert(d);
            continue;
        }
        if !d.is_empty() {
            let dec = filter.decide_dir(&d, &ap, ignore, gitignore);
            if !dec.track {
                // A notification naming something the filters exclude says so out loud instead of
                // silently doing nothing — the same `skip` event the full pass writes for it.
                skips.push((d, dec.reason.unwrap_or_default(), dec.rule.unwrap_or_default()));
                continue;
            }
        }
        starts.push((path, d));
    }
    let (mut files, mut w_skips, mut errors, mut dirs_walked, mut dirs_read) =
        walk_from(project_root, starts, filter, archive_prefixes, ignore, gitignore, count_skipped);
    skips.append(&mut w_skips);

    for rel in scope.files.iter() {
        let path = project_root.join(rel);
        let md = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                skips.push((rel.clone(), R_UNREADABLE.into(), e.to_string()));
                continue;
            }
        };
        if md.file_type().is_symlink() {
            let target = fs::read_link(&path).map(|t| t.to_string_lossy().to_string()).unwrap_or_default();
            if let Ok(a) = path.canonicalize() {
                let astr = a.to_string_lossy().replace('\\', "/");
                if ap.iter().any(|pr| !pr.is_empty() && astr.starts_with(pr.as_str())) {
                    skips.push((rel.clone(), crate::filters::R_SYMLINK_ARCHIVE.into(), target));
                    continue;
                }
            }
            files.insert(
                rel.clone(),
                CurFile { size: 0, mtime: mdts(&md), mode: util::file_mode(&md), dev: None, ino: None, kind: "symlink".into() },
            );
            continue;
        }
        if md.is_dir() {
            // Classified as a file when the scope was built but it is a directory now (the pass
            // raced a `mkdir`): it is walked as a subtree instead.
            let (mut f2, mut s2, mut e2, d2, mut r2) = walk_from(
                project_root,
                vec![(path, rel.clone())],
                filter,
                archive_prefixes,
                ignore,
                gitignore,
                count_skipped,
            );
            files.append(&mut f2);
            skips.append(&mut s2);
            errors.append(&mut e2);
            dirs_walked += d2;
            dirs_read.append(&mut r2);
            continue;
        }
        if !md.is_file() {
            continue;
        }
        let size = md.len();
        let dec: Decision = filter.decide_file(rel, size, ignore, gitignore);
        if !dec.track {
            skips.push((rel.clone(), dec.reason.unwrap_or_default(), dec.rule.unwrap_or_default()));
            continue;
        }
        let (dev, ino) = match util::file_id(&md) {
            Some((d, i)) => (Some(d), Some(i)),
            None => (None, None),
        };
        files.insert(
            rel.clone(),
            CurFile { size, mtime: mdts(&md), mode: util::file_mode(&md), dev, ino, kind: "file".into() },
        );
    }
    skips.sort();
    skips.dedup();
    ScopedWalk { files, skips, errors, dirs_walked, dirs_read, vanished }
}

fn count_files_below(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                if let Ok(md) = e.metadata() {
                    if md.is_dir() {
                        stack.push(e.path());
                    } else {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// Stable read: never store a file that is being written right now, and never read *through* a
/// symlink. The walk records symlinks as links and the scan loop skips them; this puts the same
/// rule inside the reader, so the mistake cannot return through some future caller: on a symlink
/// the open fails and the caller records the file as unreadable instead of storing the target.
pub fn read_stable(path: &Path) -> Result<Vec<u8>, String> {
    let mut last = String::from("could not read the file stably");
    for _ in 0..3 {
        let md1 = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return Err(e.to_string()),
        };
        if md1.file_type().is_symlink() {
            return Err("refusing to read through a symlink (the link is stored as a link)".into());
        }
        let data = match read_no_follow(path) {
            Ok(d) => d,
            Err(e) => return Err(e.to_string()),
        };
        let md2 = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return Err(e.to_string()),
        };
        if md1.len() == md2.len() && mdts(&md1) == mdts(&md2) {
            return Ok(data);
        }
        last = "the file changed while being read".into();
    }
    Err(last)
}

/// Open and read without following a symlink. `O_NOFOLLOW` is unix-only; elsewhere the symlink
/// check above is the only guard.
fn read_no_follow(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?
    };
    #[cfg(not(unix))]
    let mut f = fs::File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Estimate before adding a project: how many files and bytes pass the filters.
pub struct Estimate {
    pub files: usize,
    pub bytes: u64,
    pub skipped_by_reason: BTreeMap<String, usize>,
    pub top_skipped_dirs: Vec<(String, String, usize)>,
}

pub fn estimate(project_root: &Path, filter: &FilterConfig, archive_prefixes: &[PathBuf]) -> Estimate {
    let (own, git) = open_ignore_rules(project_root);
    let (files, skips, _errors) = walk(project_root, filter, archive_prefixes, &own, &git, true);
    let mut bytes = 0u64;
    for cf in files.values() {
        bytes += cf.size;
    }
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut dirs: Vec<(String, String, usize)> = Vec::new();
    for (p, r, rule) in &skips {
        *by_reason.entry(r.clone()).or_insert(0) += 1;
        if rule.contains(" files)") {
            if let Some(n) = rule
                .rsplit("(")
                .next()
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<usize>().ok())
            {
                dirs.push((p.clone(), r.clone(), n));
            }
        }
    }
    dirs.sort_by(|a, b| b.2.cmp(&a.2));
    dirs.truncate(10);
    Estimate { files: files.len(), bytes, skipped_by_reason: by_reason, top_skipped_dirs: dirs }
}

/// One observation cycle of one project.
pub fn scan_project(
    archive: &Archive,
    project: &mut Project,
    opts: &ScanOptions,
) -> Result<ScanReport, String> {
    let started = util::now_ms();
    // Optional stage timing: `PROJECTLIFE_TIMING=1` prints where the pass spent its time to stderr.
    // It exists because "the bookkeeping is cheap" is a claim, and a claim about time should be
    // auditable on the machine that makes it (see docs/LIMITATIONS.md and tools/partial_bench.py).
    let timing = std::env::var("PROJECTLIFE_TIMING").map(|v| v != "0").unwrap_or(false);
    let mut stages: Vec<(&str, i64)> = Vec::new();
    let mut lap_at = started;
    macro_rules! lap {
        ($name:expr) => {
            if timing {
                let now = util::now_ms();
                stages.push(($name, now - lap_at));
                lap_at = now;
            }
        };
    }
    lap!("start");
    let mut report = ScanReport { project: project.name.clone(), ..Default::default() };
    // FR-LIF-3: an interrupted prune is resolved before anything else touches this project.
    if let Some(what) = crate::lifecycle::recover_prune(archive, project)? {
        archive.log(&format!("{}: {what}", project.name));
    }
    let project_root = project.project_path();
    if !project_root.is_dir() {
        project.set_meta("state", Value::from("path_missing"));
        let _ = project.save_meta();
        return Err(format!(
            "project folder is unreachable: {} — state path_missing (history untouched, restore with --to still works)",
            project_root.display()
        ));
    }
    let filter = FilterConfig::for_profile(&project.profile(), &project.settings());
    let (own, git) = open_ignore_rules(&project_root);
    let arc_prefixes = vec![archive.root.clone()];
    // Round 293: a scope restricts the walk. `full` means a notification named the project root
    // itself, which is the whole project — the pass is then the ordinary one, on purpose.
    let scope = opts.scope.as_ref();
    let partial = scope.is_some();
    // A path may be written as deleted only if this pass actually looked where it should be.
    // `whole_project_walked` is true for an ordinary full pass (and for a scope that named the
    // project root); otherwise the two sets below decide it, path by path.
    let mut listed_dirs: BTreeSet<String> = BTreeSet::new();
    let mut vanished_dirs: BTreeSet<String> = BTreeSet::new();
    let whole_project_walked = scope.map(|s| s.full).unwrap_or(true);
    let (cur, skips, errors, dirs_walked) = match scope {
        Some(sc) if !sc.full => {
            let w = walk_scoped(&project_root, &filter, &arc_prefixes, &own, &git, opts.count_skipped, sc);
            listed_dirs = w.dirs_read;
            vanished_dirs = w.vanished;
            (w.files, w.skips, w.errors, w.dirs_walked)
        }
        _ => {
            let (f, s, e) = walk(&project_root, &filter, &arc_prefixes, &own, &git, opts.count_skipped);
            (f, s, e, 0usize)
        }
    };
    lap!("walk");
    report.partial = partial;
    report.scope_paths = scope.map(|s| s.paths_in).unwrap_or(0);
    report.dirs_walked = dirs_walked;
    report.files_on_disk = cur.len();
    report.errors = errors;
    report.skipped = skips.len();
    for (p, r, rule) in &skips {
        *report.skipped_by_reason.entry(r.clone()).or_insert(0) += 1;
        report.skipped_paths.push((p.clone(), r.clone(), rule.clone()));
    }

    if opts.dry_run {
        report.duration_ms = util::now_ms() - started;
        return Ok(report);
    }

    // Round 294 — what a pass reads. A full pass reads the journal and the whole cache, which is
    // what makes it the pass that proves the journal is readable at all. A partial pass reads the
    // journal's **tail** (one line) and only the cache records its notification paths cover, and
    // writes only those. `partial_walk` is false when the scope named the project root: that is the
    // whole project, and the pass then does everything the ordinary one does.
    let partial_walk = scope.map(|s| !s.full).unwrap_or(false);
    // Every pass reads the delta (it is the newest half of the state), so its size is capped in
    // bytes: past the cap a pass folds it into a new base — a few milliseconds, counted and reported
    // as `cacheCompactions` — instead of letting a long run of notification-only passes pay for a
    // growing file. A full pass always folds it. On a 10 000-file project the delta is 0 in the
    // steady state, because the periodic full pass empties it every interval.
    let delta_cap = archive.config.u64_of("cacheDeltaMaxBytes", 131072) as usize;
    let mut cache = CacheStore::open(project, !partial_walk, delta_cap)?;
    for note in cache.notes.clone() {
        archive.log(&format!("{}: {note}", project.name));
    }
    let mut journal_events: Vec<Ev> = Vec::new();
    if partial_walk {
        if !cache.had_state() {
            // No cache to read: the entries come from the journal, exactly as they did before round
            // 294. It is the slow path, it is logged, and it happens when the cache was deleted or
            // damaged — never on an ordinary cycle.
            let j = events::load_journal(&project.dir)?;
            cache.seed_from_journal(from_journal_map(&j.events));
            journal_events = j.events;
        }
    } else {
        let j = events::load_journal(&project.dir)?;
        if !cache.had_state() {
            cache.seed_from_journal(from_journal_map(&j.events));
        }
        journal_events = j.events;
    }
    lap!("journal+cache open");
    // The tail is the same for both: it is validated against the last line of the newest journal
    // file, and if it cannot be trusted it is rebuilt from a full read (and written back).
    let tail = events::tail(project)?;
    lap!("tail");
    if !partial_walk {
        // A full pass has the journal in hand: the tail is refreshed from bytes that were just read.
        let _ = events::write_tail(
            &project.dir,
            &events::Tail {
                version: 1,
                seq_last: events::next_seq(&journal_events).saturating_sub(1),
                ts_last: events::last_ts(&journal_events).unwrap_or(0),
                observed_at: events::last_observed_at(&journal_events),
                source: "full-pass".into(),
            },
        );
    }
    if let Some(prev) = Some(tail.ts_last).filter(|t| *t > 0) {
        if prev > started + 1000 {
            // Clock moved backwards: ordering is by seq, but the user must know.
            let mut e = events::ev_new(0, started, "meta");
            events::put_str(&mut e, "reason", "clock_skew");
            events::put_i64(&mut e, "previousTs", prev);
            let mut v = vec![e];
            events::append(project, &mut v)?;
            archive.log("warning: the system clock moved backwards");
        }
    }
    report.tracked_before = cache.tracked_before();
    lap!("tracked-before");
    let prev_seq = tail.seq_last;
    let batch_id = format!("b-{}", prev_seq + 1);
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut out: Vec<Ev> = Vec::new();

    let mut missing: Vec<(String, CacheEntry)> = Vec::new();
    // Round 293 — the two rules that make a partial pass safe, and the only two places where it
    // differs from a full pass:
    //
    //  (1) a tracked path may be forgotten (deleted) or replaced only if a notification covers it.
    //      Everything else is left in the cache untouched, so a change nobody mentioned is not
    //      mistaken for a deletion and the next full pass still finds the real difference.
    //  (2) only the covered part is walked, so `cur` is deliberately incomplete — and therefore
    //      `cur.is_empty()` must never be read as "the project is gone" (see `all_gone` below).
    let covered = |rel: &str| -> bool { scope.map(|s| s.covers(rel)).unwrap_or(true) };
    // "I did not look there" must never be written as "it is gone": the parent directory has to have
    // been listed in this pass, or an ancestor (or the path itself) has to be missing from disk.
    // This is what protects a directory the pass could not open (EACCES/EIO) from turning its whole
    // contents into deletions.
    let looked = |rel: &str| -> bool {
        may_write_delete(rel, &listed_dirs, &vanished_dirs, whole_project_walked)
    };
    // The old state, read the way this pass is allowed to read it: everything for a full pass, only
    // the covered records for a partial one. Round 294: this used to be the whole cache in both
    // cases, which is what a partial pass paid for with 68 ms of bookkeeping.
    if let Some(all) = cache.all_entries() {
        for (rel, entry) in all {
            if !cur.contains_key(rel) && covered(rel) && looked(rel) {
                missing.push((rel.clone(), entry.clone()));
            }
        }
    } else {
        for (rel, entry) in cache.candidates(scope)? {
            if !cur.contains_key(&rel) && covered(&rel) && looked(&rel) {
                missing.push((rel, entry));
            }
        }
    }
    let skipped_paths_set: BTreeSet<String> = skips.iter().map(|(p, _, _)| p.clone()).collect();
    // Paths hidden by filters now are not "deleted".
    missing.retain(|(p, _)| !is_under_skipped(p, &skipped_paths_set));

    lap!("candidates");
    let mut movemap: BTreeMap<String, (String, CacheEntry)> = BTreeMap::new();
    let mut used_missing: BTreeSet<String> = BTreeSet::new();
    let mut created_paths: Vec<String> = Vec::new();
    let mut changed_paths: Vec<String> = Vec::new();
    for rel in cur.keys() {
        if cache.get(rel).is_none() {
            created_paths.push(rel.clone());
        }
    }
    // 1. move detected by file identity
    for rel in created_paths.clone() {
        let cf = cur.get(rel.as_str()).unwrap();
        if let (Some(dev), Some(ino)) = (cf.dev, cf.ino) {
            if let Some((old, entry)) = missing
                .iter()
                .find(|(o, e)| !used_missing.contains(o) && e.dev == Some(dev) && e.ino == Some(ino))
            {
                used_missing.insert(old.clone());
                movemap.insert(rel.clone(), (old.clone(), entry.clone()));
            }
        }
    }

    // 2. New and changed files: read and hash.
    for rel in cur.keys() {
        let cf = cur.get(rel.as_str()).unwrap();
        if cf.kind == "symlink" {
            // Symlinks are only recorded as events (section 3); their targets are never read, and
            // this `continue` is the only place that decides it for this loop. `read_stable` also
            // refuses to follow a symlink (O_NOFOLLOW), so the rule holds even if this guard is
            // ever removed by accident — that is what the mutation control in tests/acceptance.rs
            // checks.
            continue;
        }
        let old = cache.get(rel);
        let unchanged_meta = old
            .as_ref()
            .map(|o| o.size == cf.size && o.mtime == cf.mtime && o.kind == "file")
            .unwrap_or(false);
        let is_new = old.is_none();
        if unchanged_meta && !opts.deep {
            if let Some(o) = old {
                // A full pass writes every kept entry — the base is rebuilt from them. A partial pass
                // writes only what really changed: an identity that is already right is not a reason
                // to append a delta record.
                if cache.is_full() || o.dev != cf.dev || o.ino != cf.ino {
                    let mut e = o.clone();
                    e.dev = cf.dev;
                    e.ino = cf.ino;
                    cache.set(rel, e);
                }
            }
            report.unchanged += 1;
            continue;
        }
        let abs = project_root.join(rel);
        let data = match read_stable(&abs) {
            Ok(d) => d,
            Err(err) => {
                report.unstable += 1;
                report.skipped_paths.push((rel.clone(), "unstable".into(), err.clone()));
                // The path is in `cur` but its bytes could not be read. If its identity (dev, ino)
                // matches a path that disappeared, the rename is real even though the contents are
                // not: the move is written (so the old path is not reported as deleted), no `put` is
                // written, and the last *known* version is carried to the new path. The entry keeps
                // the old size/mtime on purpose — that is what makes the next cycle re-read the
                // file instead of believing the carried-over hash.
                if let Some((oldp, entry)) = movemap.get(rel).cloned() {
                    used_missing.insert(oldp.clone());
                    let mut e = events::ev_new(0, started, "move");
                    events::put_str(&mut e, "from", &oldp);
                    events::put_str(&mut e, "to", rel);
                    events::put_str(&mut e, "batchId", &batch_id);
                    events::put_str(&mut e, "reason", &opts.reason);
                    events::put_str(&mut e, "unreadable", &err);
                    out.push(e);
                    report.moved += 1;
                    let mut f = entry.clone();
                    f.dev = cf.dev;
                    f.ino = cf.ino;
                    cache.set(rel, f);
                } else if let Some(o) = old {
                    // Carried over: in a full pass it has to be written (the base is rebuilt), in a
                    // partial pass the cache already holds it.
                    if cache.is_full() {
                        cache.set(rel, o.clone());
                    }
                }
                continue;
            }
        };
        report.bytes_read += data.len() as u64;
        let hash = store::sha256_bytes(&data);
        let mode = cf.mode;
        if let Some(o) = old {
            if o.hash == hash && o.kind == "file" {
                // Same contents: no new version, only metadata is refreshed.
                cache.set(
                    rel,
                    CacheEntry {
                        hash: hash.clone(),
                        size: cf.size,
                        mtime: cf.mtime,
                        mode,
                        kind: "file".into(),
                        target: None,
                        dev: cf.dev,
                        ino: cf.ino,
                    },
                );
                if o.mode != mode {
                    let mut e = events::ev_new(0, started, "meta");
                    events::put_str(&mut e, "reason", "mode_changed");
                    events::put_str(&mut e, "path", rel);
                    events::put_u64(&mut e, "mode", mode as u64);
                    out.push(e);
                }
                report.unchanged += 1;
                continue;
            }
        }
        if store.put(&hash, &data)? {
            report.blobs_new += 1;
        }
        // A move: first by file identity (found before reading), then by matching contents.
        let mut moved_from: Option<String> = None;
        if is_new {
            if let Some((old, _e)) = movemap.get(rel) {
                moved_from = Some(old.clone());
            } else if let Some((old, _e)) = missing
                .iter()
                .find(|(o, e)| !used_missing.contains(o) && e.hash == hash && e.size == cf.size)
            {
                used_missing.insert(old.clone());
                moved_from = Some(old.clone());
            }
        }
        if let Some(old) = moved_from {
            used_missing.insert(old.clone());
            let same_contents = cache.get(&old).map(|e| e.hash == hash).unwrap_or(false);
            let mut e = events::ev_new(0, started, "move");
            events::put_str(&mut e, "from", &old);
            events::put_str(&mut e, "to", rel);
            events::put_str(&mut e, "batchId", &batch_id);
            events::put_str(&mut e, "reason", &opts.reason);
            out.push(e);
            report.moved += 1;
            if !same_contents {
                // the file was renamed and rewritten in the same cycle: the move plus a new version
                let mut pe = events::ev_new(0, started, "put");
                events::put_str(&mut pe, "path", rel);
                events::put_str(&mut pe, "hash", &hash);
                events::put_u64(&mut pe, "size", cf.size);
                events::put_i64(&mut pe, "mtime", cf.mtime);
                events::put_u64(&mut pe, "mode", mode as u64);
                events::put_str(&mut pe, "batchId", &batch_id);
                events::put_str(&mut pe, "reason", &opts.reason);
                out.push(pe);
            }
            cache.set(
                rel,
                CacheEntry {
                    hash: hash.clone(),
                    size: cf.size,
                    mtime: cf.mtime,
                    mode,
                    kind: "file".into(),
                    target: None,
                    dev: cf.dev,
                    ino: cf.ino,
                },
            );
            continue;
        }
        if is_new {
            report.created += 1;
        } else {
            report.changed += 1;
            changed_paths.push(rel.clone());
        }
        let mut e = events::ev_new(0, started, "put");
        events::put_str(&mut e, "path", rel);
        events::put_str(&mut e, "hash", &hash);
        events::put_u64(&mut e, "size", cf.size);
        events::put_i64(&mut e, "mtime", cf.mtime);
        events::put_u64(&mut e, "mode", mode as u64);
        events::put_str(&mut e, "batchId", &batch_id);
        events::put_str(&mut e, "reason", &opts.reason);
        out.push(e);
        cache.set(
            rel,
            CacheEntry {
                hash,
                size: cf.size,
                mtime: cf.mtime,
                mode,
                kind: "file".into(),
                target: None,
                dev: cf.dev,
                ino: cf.ino,
            },
        );
    }

    // 3. Symlinks as their own events.
    for rel in cur.keys() {
        let path = project_root.join(rel);
        if let Ok(md) = fs::symlink_metadata(&path) {
            if md.file_type().is_symlink() {
                let target = fs::read_link(&path)
                    .map(|t| t.to_string_lossy().to_string())
                    .unwrap_or_default();
                let changed = cache
                    .get(rel)
                    .map(|o| o.kind != "symlink" || o.target.as_deref() != Some(target.as_str()))
                    .unwrap_or(true);
                if changed {
                    let mut e = events::ev_new(0, started, "symlink");
                    events::put_str(&mut e, "path", rel);
                    events::put_str(&mut e, "target", &target);
                    events::put_u64(&mut e, "mode", util::file_mode(&md) as u64);
                    events::put_str(&mut e, "batchId", &batch_id);
                    out.push(e);
                    report.created += 1;
                }
                cache.set(
                    rel,
                    CacheEntry {
                        hash: String::new(),
                        size: 0,
                        mtime: mdts(&md),
                        mode: util::file_mode(&md),
                        kind: "symlink".into(),
                        target: Some(target),
                        dev: None,
                        ino: None,
                    },
                );
            }
        }
    }

    // 4. Deletions (everything not already explained by a move).
    for (rel, _entry) in missing.iter() {
        // Round 294: the cache has to forget the path as well, not only the journal. A path that
        // moved away is explained by its move event, but it is gone from disk all the same — and a
        // cache that kept it would report a `delete` for it on the next pass that looks there. A full
        // pass needs no `del`: it rebuilds the base from the walk, so anything not written is dropped.
        if used_missing.contains(rel) {
            if !cache.is_full() {
                cache.del(rel);
            }
            continue;
        }
        let mut e = events::ev_new(0, started, "delete");
        events::put_str(&mut e, "path", rel);
        events::put_str(&mut e, "batchId", &batch_id);
        out.push(e);
        report.deleted += 1;
        if !cache.is_full() {
            cache.del(rel);
        }
    }

    lap!("files");
    // 5. `skip` events: written for a new (path, reason) pair or when the reason changes.
    //
    // Round 294: which pairs are already in the journal used to be found by scanning every event of
    // the journal backwards on every pass. It is now a small sorted file (`cache/skips.jsonl`) read
    // by point lookup — and a full pass seeds it from the journal it has already read, so an archive
    // from before this round does not re-emit its skips once.
    let cache_dir = project.dir.join("cache");
    let mut skip_map = cache::SkipMap::open(&cache_dir)?;
    if !journal_events.is_empty() {
        for e in journal_events.iter().rev() {
            if events::event_type(e) == "skip" {
                if let (Some(p), Some(r)) = (events::get_str(e, "path"), events::get_str(e, "reason")) {
                    skip_map.seed(&p, &r);
                }
            }
        }
    }
    for (p, r, rule) in skips.iter() {
        if skip_map.recorded(p, r)? {
            continue;
        }
        let mut e = events::ev_new(0, started, "skip");
        events::put_str(&mut e, "path", p);
        events::put_str(&mut e, "reason", r);
        if !rule.is_empty() {
            events::put_str(&mut e, "rule", rule);
        }
        out.push(e);
        skip_map.set(p, r);
    }

    // 6. Mass event.
    let touched = report.created + report.changed + report.deleted;
    let tracked = report.tracked_before.max(cur.len()).max(1);
    let percent = (touched as f64) * 100.0 / tracked as f64;
    let mass_files = archive.config.u64_of("massChangeFiles", 50);
    let mass_pct = archive.config.u64_of("massChangePercent", 30);
    let min_mass = archive.config.u64_of("minMassFiles", 3) as usize;
    let all_gone = !partial && report.tracked_before > 0 && cur.is_empty();
    let big = (touched as u64) >= mass_files || (percent >= mass_pct as f64 && touched >= min_mass);
    let mass_kind = if report.deleted > 0 && report.deleted >= report.changed + report.created {
        "mass_delete"
    } else if report.created > report.changed {
        "mass_create"
    } else {
        "mass_change"
    };
    let rewrite = report.changed >= 5 && (report.changed as f64) * 100.0 / tracked as f64 >= 80.0;
    // An initial snapshot is a known bulk operation, not an anomaly.
    let is_initial = opts.with_initial_snapshot || opts.reason == "initial";
    if !is_initial && ((big && touched > 0) || all_gone || rewrite) {
        let kind = if rewrite { "suspicious_rewrite" } else { mass_kind };
        let mut e = events::ev_new(0, started, "mass");
        events::put_str(&mut e, "kind", kind);
        events::put_u64(&mut e, "files", touched as u64);
        events::put_u64(&mut e, "deleted", report.deleted as u64);
        events::put_u64(&mut e, "changed", report.changed as u64);
        events::put_u64(&mut e, "created", report.created as u64);
        e.insert("percent".into(), Value::from((percent * 10.0).round() / 10.0));
        events::put_str(&mut e, "batchId", &batch_id);
        events::put_u64(&mut e, "lastGoodSeq", prev_seq);
        let mut sample: Vec<Value> = Vec::new();
        for (p, _) in missing.iter().take(100) {
            sample.push(Value::from(p.clone()));
        }
        for p in changed_paths.iter().take(100) {
            sample.push(Value::from(p.clone()));
        }
        for p in created_paths.iter().take(100) {
            sample.push(Value::from(p.clone()));
        }
        e.insert("samplePaths".into(), Value::Array(sample));
        report.mass = Some(e.clone());
        out.push(e);
    }

    // 7. Write: snapshot (for an initial copy) + events + state.
    if opts.with_initial_snapshot {
        let mut snap = events::ev_new(0, started, "snapshot");
        events::put_str(&mut snap, "reason", "initial");
        events::put_str(&mut snap, "batchId", &batch_id);
        events::put_u64(&mut snap, "files", cache.tracked_after() as u64);
        out.insert(0, snap);
    }
    let written = out.len() as u64;
    if !out.is_empty() {
        let mut v = out.clone();
        events::append(project, &mut v)?;
    } else {
        let _ = written;
    }
    project.set_meta("lastSeq", Value::from(prev_seq + written));
    project.set_meta("lastObservedAt", Value::from(iso_ms(started)));
    if project.state() != "paused" && project.state() != "error" {
        project.set_meta("state", Value::from("active"));
    }
    let mut intervals: Vec<i64> = project
        .meta
        .get("observedIntervalsMs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default();
    if let Some(prev) = tail.observed_at {
        let delta = started - prev;
        if delta > 0 && delta < 24 * 3_600_000 {
            intervals.push(delta);
            if intervals.len() > 200 {
                let drop = intervals.len() - 200;
                intervals.drain(0..drop);
            }
        }
    }
    // The list stays in chronological order and the truncation drops by POSITION, so the oldest
    // sample is the one that leaves. Sorting it in place first (as this code did) made `drain(0..)`
    // drop the smallest values instead of the oldest ones, and the measurement window silently
    // drifted towards the shortest interval ever seen. median/p95 are computed on a sorted copy.
    let (med, p95) = if intervals.is_empty() {
        (0, 0)
    } else {
        let mut sorted = intervals.clone();
        util::median_p95(&mut sorted).unwrap_or((0, 0))
    };
    project.set_meta(
        "observedIntervalsMs",
        Value::Array(intervals.iter().map(|x| Value::from(*x)).collect()),
    );
    let mut iv = Map::new();
    iv.insert("median".into(), Value::from(med));
    iv.insert("p95".into(), Value::from(p95));
    iv.insert("samples".into(), Value::from(intervals.len() as u64));
    project.set_meta("observedIntervalMs", Value::Object(iv));
    lap!("events built");
    cache.commit(project)?;
    skip_map.commit()?;
    lap!("commit");
    report.cache_bytes_read = cache.bytes_read();
    report.cache_base_rewrites = cache.stats.base_rewrites;
    report.cache_delta_records = cache.stats.delta_records;
    report.cache_delta_records_before = cache.stats.delta_records_before;
    report.cache_compactions = cache.stats.compactions;
    report.cache_rebuilt = cache.stats.rebuilt_from_journal;
    report.journal_full_bytes_read = events::JOURNAL_FULL_BYTES_READ.load(std::sync::atomic::Ordering::Relaxed);
    report.journal_tail_bytes_read = events::JOURNAL_TAIL_BYTES_READ.load(std::sync::atomic::Ordering::Relaxed);
    report.skip_map_bytes_read = skip_map.bytes_read;
    // The cycle's own metadata is derived (see `Project::save_meta_lazy`): the durable path is for
    // the commands that change real state.
    let _ = project.save_meta_lazy();
    report.duration_ms = util::now_ms() - started;
    lap!("meta");
    if timing {
        let parts: Vec<String> = stages.iter().map(|(n, ms)| format!("{n} {ms}ms")).collect();
        eprintln!("timing[{}]: {}  total {}ms", project.name, parts.join(", "), report.duration_ms);
        let _ = lap_at;
    }
    Ok(report)
}

/// One **partial** observation cycle: the same pass as `scan_project`, restricted to the paths the
/// notifications named (round 293).
///
/// This is the entry point the daemon uses for a notification-driven pass and the one `pl
/// partial-pass` offers, so a partial pass can be run and measured from outside the process. It
/// writes exactly the same events, in the same order, through the same code — it only walks less.
pub fn scan_project_partial(
    archive: &Archive,
    project: &mut Project,
    paths: &[String],
    reason: &str,
) -> Result<ScanReport, String> {
    if paths.is_empty() {
        return Err("a partial pass needs at least one notification path".into());
    }
    let project_root = project.project_path();
    let scope = Scope::from_paths(&project_root, paths);
    let opts = ScanOptions {
        reason: reason.to_string(),
        deep: false,
        verbose_filters: false,
        dry_run: false,
        with_initial_snapshot: false,
        count_skipped: false,
        scope: Some(scope),
    };
    scan_project(archive, project, &opts)
}

/// May a tracked path that is not on disk be written as a `delete` by this pass?
///
/// `whole_project_walked` is true for an ordinary full pass, where the question is answered by the
/// walk itself. Otherwise the answer is yes only when the pass really looked: the path's parent
/// directory was listed in this pass (`listed`), or the path or one of its ancestors is missing from
/// disk (`vanished` — a deleted file, a deleted folder, or a folder that is no longer a directory).
///
/// The third case — covered by the notification, parent not listed, nothing vanished — is the one
/// this rule exists for: it is a directory the pass could not open, and "I could not look" must never
/// be recorded as "it is gone".
pub fn may_write_delete(
    rel: &str,
    listed: &BTreeSet<String>,
    vanished: &BTreeSet<String>,
    whole_project_walked: bool,
) -> bool {
    if whole_project_walked {
        return true;
    }
    let parent = match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    };
    if listed.contains(parent) {
        return true;
    }
    let mut cur = rel;
    loop {
        if vanished.contains(cur) {
            return true;
        }
        match cur.rfind('/') {
            Some(i) => cur = &cur[..i],
            None => break,
        }
    }
    false
}

fn is_under_skipped(path: &str, skipped: &BTreeSet<String>) -> bool {
    if skipped.contains(path) {
        return true;
    }
    let mut cur = path.to_string();
    while let Some(idx) = cur.rfind('/') {
        cur.truncate(idx);
        if skipped.contains(&cur) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The rule that keeps a partial pass from mistaking "I did not look" for "it is gone".
    ///
    /// The end-to-end case this protects (a directory the process cannot open, EACCES/EIO) cannot be
    /// produced in this test suite: the suite runs as root, and root opens a `chmod 000` directory.
    /// So the decision itself is a function, and the function is what is tested here.
    #[test]
    fn a_path_may_be_deleted_only_where_the_pass_looked() {
        let nothing = set(&[]);
        // an ordinary full pass: the walk is the evidence
        assert!(may_write_delete("src/a.txt", &nothing, &nothing, true));
        // the parent directory was listed in this pass
        assert!(may_write_delete("src/a.txt", &set(&["src"]), &nothing, false));
        // the path itself is gone from disk (a deleted file that was named)
        assert!(may_write_delete("src/a.txt", &nothing, &set(&["src/a.txt"]), false));
        // an ancestor is gone (a deleted folder)
        assert!(may_write_delete("src/deep/a.txt", &nothing, &set(&["src"]), false));
        // …but "covered by the notification, never looked at" is not a deletion
        assert!(!may_write_delete("src/deep/a.txt", &set(&["other"]), &nothing, false));
        assert!(!may_write_delete("src/deep/a.txt", &nothing, &nothing, false));
        // the top level of the project is listed as "" and no parent has to be listed for it
        assert!(may_write_delete("a.txt", &set(&[""]), &nothing, false));
    }

    /// The scope decides what a partial pass may touch: the named paths and what lies under the
    /// named directories — never a sibling the notification did not mention.
    #[test]
    fn a_scope_covers_the_named_paths_and_the_subtrees_they_name() {
        let mut sc = Scope { paths_in: 2, ..Default::default() };
        sc.dirs.insert("src".into());
        sc.files.insert("top.txt".into());
        assert!(sc.covers("src"), "the named directory itself");
        assert!(sc.covers("src/a.txt"), "a file under it");
        assert!(sc.covers("src/deep/a.txt"), "and deeper");
        assert!(sc.covers("top.txt"), "a named file");
        assert!(!sc.covers("other.txt"), "an unmentioned sibling");
        assert!(!sc.covers("srcx/a.txt"), "a name that only starts the same way");
        // a path that merely begins with the same text is not under the directory (the separator is
        // part of the comparison), but anything really nested under `src` is
        assert!(!sc.covers("src-other/a.txt"));
        assert!(sc.covers("src/a.txt.bak/x"), "textually nested under a covered directory");
        // a scope that named the project root covers everything
        let full = Scope { full: true, ..Default::default() };
        assert!(full.covers("anything/at/all.txt"));
    }

    /// A notification path is normalised before it is compared with a recorded path.
    #[test]
    fn notification_paths_are_normalised() {
        assert_eq!(normalize_rel("/src//a.txt/"), "src/a.txt");
        assert_eq!(normalize_rel("./src/a.txt"), "src/a.txt");
        assert_eq!(normalize_rel("src/a.txt"), "src/a.txt");
        assert_eq!(normalize_rel(""), "");
    }
}
