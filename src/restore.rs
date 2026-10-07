//! Restore: plan (preview) and write. Atomicity and "no empty files" are not good intentions but
//! checkable properties: a blob is read and verified against its sha256 before anything is written.

use crate::archive::{Archive, Project};
use crate::events::{self, FileSt};
use crate::store::Store;
use crate::util;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub at: i64,
    pub at_label: String,
    pub paths: Vec<String>,
    pub to: Option<PathBuf>,
    pub into_project: bool,
    pub clean: bool,
    pub preview: bool,
    /// Round 300: "give back what is missing". Create only the files that are absent on disk right
    /// now, and never overwrite or delete anything. This is the repair a person wants after an agent
    /// deleted part of a project: everything they have written by hand since survives untouched.
    pub missing: bool,
}

#[derive(Clone, Debug)]
pub struct RestorePlan {
    pub target: PathBuf,
    pub moment: i64,
    pub at_label: String,
    pub create: usize,
    pub overwrite: usize,
    pub delete_extra: usize,
    /// Files that exist at the moment and are already on disk, so `--missing` leaves them alone.
    pub present: usize,
    pub bytes: u64,
    pub missing_blobs: Vec<String>,
    pub symlinks: Vec<String>,
    pub warnings: Vec<String>,
    /// True when this plan was built in create-only repair mode (`--missing`).
    pub missing: bool,
    pub files: Vec<(String, FileSt)>,
    pub extra: Vec<String>,
}

impl RestorePlan {
    pub fn total(&self) -> usize {
        self.create + self.overwrite
    }
}

fn target_dir(project: &Project, to: &Option<PathBuf>, into_project: bool) -> Result<PathBuf, String> {
    if into_project {
        return Ok(project.project_path());
    }
    if let Some(t) = to {
        return Ok(t.clone());
    }
    let root = project.project_path();
    let parent = root.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
    let stamp = util::fmt_local(util::now_ms()).replace(['-', ' ', ':'], "");
    Ok(parent.join(format!("{name}-restored-{stamp}")))
}

/// The restore plan: what will be created, overwritten or deleted, and what is missing.
pub fn plan(archive: &Archive, project: &Project, opts: &RestoreOptions) -> Result<RestorePlan, String> {
    let journal = events::load_journal(&project.dir)?;
    let hstart = project.history_starts_at();
    if hstart > 0 && opts.at < hstart {
        let (m, h) = util::fmt_moment_pair(opts.at, hstart);
        return Err(format!(
            "moment {} is earlier than the start of the available history {}.\nAvailable range: {} … {}",
            m,
            h,
            util::fmt_local(hstart),
            util::fmt_local(events::last_observed_at(&journal.events).unwrap_or(util::now_ms()))
        ));
    }
    let mut state = events::state_at(&journal.events, opts.at, None);
    if !opts.paths.is_empty() {
        let want: Vec<String> = opts.paths.iter().map(|p| util::norm_rel(p)).collect();
        state.retain(|k, _| want.iter().any(|w| k == w || k.starts_with(&format!("{w}/"))));
    }
    let target = target_dir(project, &opts.to, opts.into_project)?;
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut create = 0;
    let mut overwrite = 0;
    let mut present = 0;
    let mut bytes = 0u64;
    let mut missing_blobs = Vec::new();
    let mut symlinks = Vec::new();
    let mut warnings = Vec::new();
    let mut files = Vec::new();
    if opts.missing && opts.clean {
        return Err("--missing only creates; --clean deletes. Choose one: --missing never touches a file that exists".into());
    }
    for (rel, f) in state.iter() {
        if !util::is_safe_rel(rel) {
            warnings.push(format!("path escapes the target folder, skipped: {rel}"));
            continue;
        }
        let dst = target.join(rel);
        if opts.missing && fs::symlink_metadata(&dst).is_ok() {
            // Something already occupies this path — a file, a folder or a link. Repair leaves it
            // exactly as it is; that is the whole point of the mode.
            present += 1;
            continue;
        }
        if f.kind == "symlink" {
            let inside = f
                .target
                .as_deref()
                .map(|t| !Path::new(t).is_absolute() && util::is_safe_rel(t))
                .unwrap_or(false);
            if inside {
                symlinks.push(rel.clone());
            } else {
                warnings.push(format!(
                    "symlink {rel} -> {} is not recreated: the target is outside the project",
                    f.target.clone().unwrap_or_else(|| "?".into())
                ));
            }
            if dst.exists() {
                overwrite += 1;
            } else {
                create += 1;
            }
            files.push((rel.clone(), f.clone()));
            continue;
        }
        if !store.has(&f.hash) {
            missing_blobs.push(rel.clone());
        } else {
            bytes += f.size;
        }
        if dst.exists() {
            overwrite += 1;
        } else {
            create += 1;
        }
        files.push((rel.clone(), f.clone()));
    }
    let mut extra = Vec::new();
    if opts.clean && !opts.paths.is_empty() {
        warnings.push("--clean together with --path is not supported: deletion happens on a full restore only".into());
    }
    if opts.clean && opts.paths.is_empty() && target.is_dir() {
        let filter = crate::filters::FilterConfig::for_profile(&project.profile(), &project.settings());
        let (own, git) = crate::scan::open_ignore_rules(&target);
        let none: Vec<PathBuf> = Vec::new();
        let (cur, _skips, _errs) = crate::scan::walk(&target, &filter, &none, &own, &git, false);
        for rel in cur.keys() {
            if !state.contains_key(rel) {
                extra.push(rel.clone());
            }
        }
    }
    for (from, to, reason) in events::gaps(&journal.events) {
        if opts.at >= from && opts.at <= to {
            warnings.push(format!(
                "the program was not observing the project then ({} … {}, {}): the nearest previous known state is shown",
                util::fmt_local(from),
                util::fmt_local(to),
                reason
            ));
        }
    }
    let _ = archive;
    Ok(RestorePlan {
        target,
        moment: opts.at,
        at_label: opts.at_label.clone(),
        create,
        overwrite,
        delete_extra: extra.len(),
        present,
        bytes,
        missing_blobs,
        symlinks,
        warnings,
        missing: opts.missing,
        files,
        extra,
    })
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub restored: usize,
    pub failed: usize,
    pub deleted: usize,
    pub skipped_symlinks: usize,
    /// Repair mode only: paths that appeared on disk between the plan and the write, and were
    /// therefore left alone. Nothing may be overwritten by `--missing`, not even in this window.
    pub skipped_present: usize,
    /// What was actually written, so a caller can verify it without parsing prose. Round 300.
    pub restored_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub missing: Vec<String>,
    pub bytes: u64,
}

/// Execute a plan. Nothing is written when a blob is absent or fails its hash check.
pub fn execute(project: &Project, plan: &RestorePlan, opts: &RestoreOptions) -> Result<RestoreReport, String> {
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut report = RestoreReport {
        warnings: plan.warnings.clone(),
        missing: plan.missing_blobs.clone(),
        ..Default::default()
    };
    fs::create_dir_all(&plan.target).map_err(|e| format!("cannot create {}: {e}", plan.target.display()))?;
    for (rel, f) in &plan.files {
        let dst = plan.target.join(rel);
        if opts.missing && fs::symlink_metadata(&dst).is_ok() {
            // The plan said this path was empty; something has appeared since. Repair creates and
            // never overwrites, so the file that is there now wins.
            report.skipped_present += 1;
            continue;
        }
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                report.failed += 1;
                report.warnings.push(format!("{rel}: {e}"));
                continue;
            }
        }
        if f.kind == "symlink" {
            let Some(target) = f.target.clone() else { continue };
            let inside = !Path::new(&target).is_absolute() && util::is_safe_rel(&target);
            if !inside {
                report.skipped_symlinks += 1;
                continue;
            }
            let _ = fs::remove_file(&dst);
            #[cfg(unix)]
            {
                if let Err(e) = std::os::unix::fs::symlink(&target, &dst) {
                    report.failed += 1;
                    report.warnings.push(format!("{rel}: symlink not created: {e}"));
                } else {
                    report.restored += 1;
                    report.restored_paths.push(rel.clone());
                }
            }
            #[cfg(not(unix))]
            {
                report.skipped_symlinks += 1;
                report.warnings.push(format!("{rel}: symlinks are not supported on this OS"));
            }
            continue;
        }
        let data = match store.read_verified(&f.hash) {
            Ok(d) => d,
            Err(e) => {
                report.failed += 1;
                report.missing.push(rel.clone());
                report.warnings.push(format!("{rel}: {e}"));
                continue;
            }
        };
        if let Err(e) = util::write_atomic(&dst, &data) {
            report.failed += 1;
            report.warnings.push(format!("{rel}: {e}"));
            continue;
        }
        if f.mode != 0 {
            util::set_mode(&dst, f.mode);
        }
        report.restored += 1;
        report.restored_paths.push(rel.clone());
        report.bytes += data.len() as u64;
    }
    if opts.clean && opts.paths.is_empty() {
        for rel in &plan.extra {
            if !util::is_safe_rel(rel) {
                continue;
            }
            let p = plan.target.join(rel);
            if fs::remove_file(&p).is_ok() {
                report.deleted += 1;
            }
        }
    }
    Ok(report)
}

/// "Last good state": the moment immediately before the last mass event.
pub fn last_good(events_list: &[events::Ev], _project: &Project) -> Option<(i64, String)> {
    let mass_idx: Vec<usize> = events_list
        .iter()
        .enumerate()
        .filter(|(_, e)| events::event_type(e) == "mass")
        .map(|(i, _)| i)
        .collect();
    let last = *mass_idx.last()?;
    let mut first = last;
    let mut i = last;
    while i > 0 {
        let prev = i - 1;
        if events::event_type(&events_list[prev]) != "mass" {
            break;
        }
        if events::ts_of(&events_list[i]) - events::ts_of(&events_list[prev]) < 600_000 {
            first = prev;
            i = prev;
        } else {
            break;
        }
    }
    let mass_ts = events::ts_of(&events_list[first]);
    // The mass event records `lastGoodSeq`: the last sequence number before this cycle, which is the
    // boundary between "everything was fine" and the anomaly. If it is missing, fall back to
    // excluding every event that shares the mass event's batch.
    let good_seq = events::get_u64(&events_list[first], "lastGoodSeq");
    let batch = events::get_str(&events_list[first], "batchId");
    let is_observation = |e: &events::Ev| {
        matches!(events::event_type(e), "put" | "delete" | "move" | "symlink" | "snapshot")
    };
    let mut candidates: Vec<i64> = Vec::new();
    for (i, e) in events_list.iter().enumerate() {
        if i >= first || !is_observation(e) {
            continue;
        }
        match good_seq {
            Some(seq) => {
                if events::seq_of(e) <= seq {
                    candidates.push(events::ts_of(e));
                }
            }
            None => {
                let same_batch = match (&batch, events::get_str(e, "batchId")) {
                    (Some(b), Some(eb)) => *b == eb,
                    _ => false,
                };
                if !same_batch {
                    candidates.push(events::ts_of(e));
                }
            }
        }
    }
    let best = candidates.into_iter().max()?;
    let kind = events::get_str(&events_list[first], "kind").unwrap_or_else(|| "mass".into());
    let files = events::get_u64(&events_list[first], "files").unwrap_or(0);
    Some((
        best,
        format!("mass event {kind} ({files} files) at {}", util::fmt_local(mass_ts)),
    ))
}
