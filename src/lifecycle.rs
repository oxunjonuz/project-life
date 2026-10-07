//! History lifecycle: prune with anchors, export, import, export-and-prune, archive-delete.
//!
//! Automatic pruning is forbidden: nothing in this module ever runs on its own.

use crate::archive::{iso_ms, Archive, Project};
use crate::events::{self, Ev};
use crate::store::{self, Store};
use crate::util;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct PrunePlan {
    pub versions_before: usize,
    pub blobs_before: usize,
    pub bytes_before: u64,
    pub versions_after: usize,
    pub blobs_after: usize,
    pub anchors: usize,
    pub keep_events: usize,
    pub history_starts_at: i64,
    pub files_available_after: usize,
}

/// Deterministic crash point for the recovery tests.
///
/// With `PROJECTLIFE_CRASH_AFTER=<phase>` the process is killed with SIGKILL at that exact point —
/// a real `kill -9`, not a clean exit and not an error return — so the test can inspect what a
/// crash at that moment leaves on disk. Unset (the normal case) it does nothing.
pub fn crash_if(phase: &str) {
    if let Ok(want) = std::env::var("PROJECTLIFE_CRASH_AFTER") {
        if want == phase {
            eprintln!("PROJECTLIFE_CRASH_AFTER={phase}: killing the process with SIGKILL now");
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
                std::process::exit(137);
            }
            #[cfg(not(unix))]
            {
                std::process::abort();
            }
        }
    }
}

/// Delete every blob the journal does not reference. Idempotent: safe to call again after a crash.
fn delete_unreferenced_blobs(project: &Project) -> Result<usize, String> {
    let journal = events::load_journal(&project.dir)?;
    let refs: BTreeSet<String> = journal
        .events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .filter_map(|e| events::get_str(e, "hash"))
        .collect();
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut removed = 0usize;
    for (h, _sz) in store.list_all() {
        if !refs.contains(&h) && fs::remove_file(store::blob_path(&project.blobs_dir(), &h)).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn newest_dir_with_prefix(parent: &Path, prefix: &str) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with(prefix)).unwrap_or(false))
        .collect();
    hits.sort();
    hits.pop()
}

fn remove_stale_prune_dirs(project: &Project) {
    let tmp = project.tmp_dir();
    for prefix in ["events.new-", "events.old-", "events.new", "events.old"] {
        while let Some(d) = newest_dir_with_prefix(&tmp, prefix) {
            if fs::remove_dir_all(&d).is_err() {
                break;
            }
        }
    }
}

/// Resolve an interrupted `prune` (FR-LIF-3). Called before every project operation that writes
/// or reports state, so that a crash between the two renames, after the metadata save or in the
/// middle of blob deletion never leaves the project in a half-swapped state.
///
/// `Ok(None)` — nothing was pending. `Ok(Some(text))` — a pending prune was completed or rolled
/// back; the text says which. The journal is never rewritten here: either the old journal is put
/// back in place, or the new one is kept and the metadata and blobs are brought in line with it.
pub fn recover_prune(archive: &Archive, project: &mut Project) -> Result<Option<String>, String> {
    let phase_file = project.prune_journal();
    if !phase_file.is_file() {
        // Leftovers can exist without a phase file only from an older build; tidy them silently.
        remove_stale_prune_dirs(project);
        return Ok(None);
    }
    let text = fs::read_to_string(&phase_file).map_err(|e| format!("{}: {e}", phase_file.display()))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let phase = v.get("phase").and_then(|x| x.as_str()).unwrap_or("start").to_string();
    let before = v.get("before").and_then(|x| x.as_i64()).unwrap_or_else(|| project.history_starts_at());
    let action = match phase.as_str() {
        "start" => {
            remove_stale_prune_dirs(project);
            "rolled back (nothing had been swapped yet)".to_string()
        }
        "journal_built" => {
            // The new journal was built but not swapped in — unless the crash landed between the
            // two renames, in which case the events directory is missing and the old one waits.
            if !project.events_dir().is_dir() {
                if let Some(old) = newest_dir_with_prefix(&project.tmp_dir(), "events.old-") {
                    fs::rename(&old, project.events_dir()).map_err(|e| format!("rollback: {e}"))?;
                    util::sync_dir(&project.dir);
                }
            }
            remove_stale_prune_dirs(project);
            "rolled back: the journal from before the prune is back in place".to_string()
        }
        "journal_swapped" => {
            let journal = events::load_journal(&project.dir)?;
            let last = events::next_seq(&journal.events).saturating_sub(1);
            project.set_meta("lastSeq", Value::from(last));
            project.set_meta("historyStartsAt", Value::from(iso_ms(before)));
            project.save_meta()?;
            let removed = delete_unreferenced_blobs(project)?;
            remove_stale_prune_dirs(project);
            format!("completed: the swapped journal was kept, metadata corrected, {removed} blobs deleted")
        }
        _ => {
            let removed = delete_unreferenced_blobs(project)?;
            remove_stale_prune_dirs(project);
            format!("completed: history was already swapped, {removed} blobs deleted")
        }
    };
    let _ = fs::remove_file(&phase_file);
    archive.log(&format!("prune recovery for {}: {action} (phase {phase})", project.name));
    Ok(Some(format!("prune was interrupted at phase '{phase}' — {action}")))
}

/// What pruning would do: what stays, what goes, how much space comes back.
pub fn prune_plan(project: &Project, before: i64) -> Result<PrunePlan, String> {
    let journal = events::load_journal(&project.dir)?;
    let hstart = project.history_starts_at();
    if before <= hstart {
        return Err(format!(
            "prune moment {} is not later than the start of history {} — nothing to prune",
            util::fmt_local(before),
            util::fmt_local(hstart)
        ));
    }
    let state = events::state_at(&journal.events, before, None);
    let keep: Vec<Ev> = journal.events.iter().filter(|e| events::ts_of(e) > before).cloned().collect();
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for f in state.values() {
        if !f.hash.is_empty() {
            refs.insert(f.hash.clone());
        }
    }
    for e in &keep {
        if events::event_type(e) == "put" {
            if let Some(h) = events::get_str(e, "hash") {
                refs.insert(h);
            }
        }
    }
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let all = store.list_all();
    let bytes_before: u64 = all.iter().map(|(_, s)| *s).sum();
    let blobs_after = all.iter().filter(|(h, _)| refs.contains(h)).count();
    let put_before = journal.events.iter().filter(|e| events::event_type(e) == "put").count();
    let keep_puts = keep.iter().filter(|e| events::event_type(e) == "put").count();
    Ok(PrunePlan {
        versions_before: put_before,
        blobs_before: all.len(),
        bytes_before,
        versions_after: state.len() + keep_puts,
        blobs_after,
        anchors: state.len(),
        keep_events: keep.len(),
        history_starts_at: before,
        files_available_after: state.len(),
    })
}

/// Prune. Order: build the new journal in a temporary folder → fsync → atomic swap → state → delete
/// unreferenced blobs. An interrupted operation either rolls back or completes on the next run,
/// guided by `prune.journal`.
pub fn prune(archive: &Archive, project: &mut Project, before: i64) -> Result<PrunePlan, String> {
    let plan = prune_plan(project, before)?;
    let journal = events::load_journal(&project.dir)?;
    let state = events::state_at(&journal.events, before, None);
    let keep: Vec<Ev> = journal.events.iter().filter(|e| events::ts_of(e) > before).cloned().collect();

    // 1. The new journal: one anchor snapshot + a put for every file at T, then the events after T.
    let mut new_events: Vec<Ev> = Vec::new();
    {
        let mut snap = events::ev_new(0, before, "snapshot");
        events::put_str(&mut snap, "reason", "prune-anchor");
        events::put_u64(&mut snap, "files", state.len() as u64);
        new_events.push(snap);
        for (rel, f) in state.iter() {
            if f.kind == "symlink" {
                let mut e = events::ev_new(0, before, "symlink");
                events::put_str(&mut e, "path", rel);
                events::put_str(&mut e, "target", f.target.as_deref().unwrap_or(""));
                events::put_u64(&mut e, "mode", f.mode as u64);
                events::put_str(&mut e, "reason", "prune-anchor");
                new_events.push(e);
            } else {
                let mut e = events::ev_new(0, before, "put");
                events::put_str(&mut e, "path", rel);
                events::put_str(&mut e, "hash", &f.hash);
                events::put_u64(&mut e, "size", f.size);
                events::put_i64(&mut e, "mtime", f.ts);
                events::put_u64(&mut e, "mode", f.mode as u64);
                events::put_str(&mut e, "reason", "prune-anchor");
                new_events.push(e);
            }
        }
    }
    for e in &keep {
        new_events.push(e.clone());
    }
    swap_journal(archive, project, new_events, before, &format!("prune {}", project.name))?;
    // Recorded here rather than in the CLI, so that every path that prunes (`prune --before`,
    // `export-and-prune`) leaves the same trace for `recent`, `undo` and `suggest` to read.
    crate::ops::record(
        archive,
        "prune",
        &project.name,
        serde_json::json!({
            "before": before,
            "beforeLabel": util::fmt_local(before),
            "versionsBefore": plan.versions_before,
            "versionsAfter": plan.versions_after,
            "blobsBefore": plan.blobs_before,
            "blobsAfter": plan.blobs_after,
        }),
    );
    Ok(plan)
}

/// Replace the project's history with a prepared journal, then delete the blobs it no longer
/// references. This is the one place in the program that replaces history, so it is also the one
/// place with the crash points the recovery tests use: an interrupted call is finished or rolled
/// back by `recover_prune` on the next ordinary operation (FR-LIF-3). `before` is the new start of
/// history and it is written into the phase file, so a crash after the swap cannot lose it.
///
/// Both callers use it: `prune --before` (one anchor at the boundary) and `prune --policy`
/// (an anchor for every retained bucket). The crash points live here and only here.
pub fn swap_journal(
    archive: &Archive,
    project: &mut Project,
    mut new_events: Vec<Ev>,
    before: i64,
    label: &str,
) -> Result<usize, String> {
    // Order is preserved; values become contiguous.
    for (i, e) in new_events.iter_mut().enumerate() {
        e.insert("seq".into(), Value::from((i + 1) as u64));
    }
    let stamp = util::now_ms();
    let phase_file = project.prune_journal();
    let project_id = project.id.clone();
    let write_phase = |phase: &str, extra: &str| {
        let body = format!(
            "{{\"phase\":\"{phase}\",\"stamp\":{stamp},\"before\":{before},\"project\":\"{project_id}\",\"extra\":{}}}\n",
            serde_json::to_string(extra).unwrap_or_else(|_| "\"\"".into())
        );
        let _ = util::write_atomic(&phase_file, body.as_bytes());
    };
    write_phase("start", "");
    crash_if("start");

    let tmp_events = project.tmp_dir().join(format!("events.new-{stamp}"));
    let _ = fs::remove_dir_all(&tmp_events);
    fs::create_dir_all(&tmp_events).map_err(|e| e.to_string())?;
    {
        let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in &new_events {
            by_month
                .entry(util::month_name(events::ts_of(e)))
                .or_default()
                .push(serde_json::to_string(&Value::Object(e.clone())).map_err(|e| e.to_string())?);
        }
        for (m, lines) in by_month {
            let body = lines.join("\n") + "\n";
            let p = tmp_events.join(format!("{m}.jsonl"));
            util::write_atomic(&p, body.as_bytes()).map_err(|e| e.to_string())?;
        }
        util::sync_dir(&tmp_events);
    }
    write_phase("journal_built", &tmp_events.display().to_string());
    crash_if("journal_built");

    // Swap the journal directory.
    let events_dir = project.events_dir();
    let old_dir = project.tmp_dir().join(format!("events.old-{stamp}"));
    let _ = fs::remove_dir_all(&old_dir);
    if events_dir.exists() {
        fs::rename(&events_dir, &old_dir).map_err(|e| format!("journal swap: {e}"))?;
    }
    // Between the two renames the project has no events directory at all: the recovery path has to
    // know that this window exists, so the test crashes here on purpose.
    crash_if("first_rename");
    fs::rename(&tmp_events, &events_dir).map_err(|e| format!("journal swap: {e}"))?;
    util::sync_dir(&project.dir);
    write_phase("journal_swapped", "");
    crash_if("journal_swapped");

    project.set_meta("lastSeq", Value::from(new_events.len() as u64));
    project.set_meta("historyStartsAt", Value::from(iso_ms(before)));
    project.save_meta()?;
    // The journal was replaced: the tail describes a journal that no longer exists. Forgetting it is
    // cheaper and safer than rewriting it here (the next reader rebuilds it from the journal).
    events::invalidate_tail(&project.dir);
    write_phase("meta_saved", "");
    crash_if("meta_saved");
    let _ = fs::remove_dir_all(&old_dir);

    // Delete unreferenced blobs.
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let refs: BTreeSet<String> = new_events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .filter_map(|e| events::get_str(e, "hash"))
        .collect();
    let mut removed = 0usize;
    for (h, _sz) in store.list_all() {
        if !refs.contains(&h) {
            let p = store::blob_path(&project.blobs_dir(), &h);
            if fs::remove_file(&p).is_ok() {
                removed += 1;
                crash_if("blobs_deleting");
            }
        }
    }
    write_phase("blobs_removed", &format!("{removed}"));
    let _ = fs::remove_file(&phase_file);
    archive.log(&format!("{label}: {removed} blobs deleted, history starts at {}", util::fmt_local(before)));
    Ok(removed)
}

pub struct ExportReport {
    pub out: PathBuf,
    pub files: usize,
    pub blobs: usize,
    pub bytes: u64,
    pub verified: bool,
    pub manifest: PathBuf,
}

/// Export a range: a self-sufficient copy (anchors at `from` + events + blobs + manifest).
pub fn export(
    archive: &Archive,
    project: &Project,
    from: Option<i64>,
    to: Option<i64>,
    out: &Path,
) -> Result<ExportReport, String> {
    let journal = events::load_journal(&project.dir)?;
    let from_ms = from.unwrap_or_else(|| project.history_starts_at());
    let to_ms = to.unwrap_or_else(|| events::last_observed_at(&journal.events).unwrap_or(util::now_ms()));
    fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let blobs_out = out.join("blobs");
    let events_out = out.join("events");
    fs::create_dir_all(&blobs_out).map_err(|e| e.to_string())?;
    fs::create_dir_all(&events_out).map_err(|e| e.to_string())?;
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());

    let mut new_events: Vec<Ev> = Vec::new();
    let state = events::state_at(&journal.events, from_ms, None);
    let mut snap = events::ev_new(0, from_ms, "snapshot");
    events::put_str(&mut snap, "reason", "export-anchor");
    events::put_u64(&mut snap, "files", state.len() as u64);
    new_events.push(snap);
    for (rel, f) in state.iter() {
        if f.kind == "symlink" {
            let mut e = events::ev_new(0, from_ms, "symlink");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "target", f.target.as_deref().unwrap_or(""));
            new_events.push(e);
        } else {
            let mut e = events::ev_new(0, from_ms, "put");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "hash", &f.hash);
            events::put_u64(&mut e, "size", f.size);
            events::put_u64(&mut e, "mode", f.mode as u64);
            events::put_i64(&mut e, "mtime", f.ts);
            new_events.push(e);
        }
    }
    for e in journal.events.iter() {
        let ts = events::ts_of(e);
        if ts > from_ms && ts <= to_ms {
            new_events.push(e.clone());
        }
    }
    for (i, e) in new_events.iter_mut().enumerate() {
        e.insert("seq".into(), Value::from((i + 1) as u64));
    }

    let mut refs: BTreeSet<String> = BTreeSet::new();
    for e in &new_events {
        if events::event_type(e) == "put" {
            if let Some(h) = events::get_str(e, "hash") {
                refs.insert(h);
            }
        }
    }
    let mut bytes = 0u64;
    let mut copied = 0usize;
    for h in &refs {
        let data = match store.read_verified(h) {
            Ok(d) => d,
            Err(e) => {
                // A half-written export directory must never look trustworthy: mark it plainly.
                let _ = util::write_atomic(
                    &out.join("EXPORT_FAILED.txt"),
                    format!(
                        "EXPORT FAILED — do not rely on this directory.\nBlob {h} could not be verified: {e}\nNothing was pruned.\n"
                    )
                    .as_bytes(),
                );
                return Err(format!("export stopped: blob {h} did not verify ({e}); nothing was pruned"));
            }
        };
        let dst = store::blob_path(&blobs_out, h);
        fs::create_dir_all(dst.parent().unwrap_or(&blobs_out)).map_err(|e| e.to_string())?;
        util::write_atomic(&dst, &data).map_err(|e| e.to_string())?;
        bytes += data.len() as u64;
        copied += 1;
    }
    let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &new_events {
        by_month
            .entry(util::month_name(events::ts_of(e)))
            .or_default()
            .push(serde_json::to_string(&Value::Object(e.clone())).map_err(|e| e.to_string())?);
    }
    for (m, lines) in by_month {
        util::write_atomic(&events_out.join(format!("{m}.jsonl")), (lines.join("\n") + "\n").as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let export_meta = serde_json::json!({
        "schemaVersion": 1,
        "kind": "projectlife-export",
        "projectId": project.id,
        "projectName": project.name,
        "projectRoot": project.project_root,
        "profile": project.profile(),
        "settings": project.settings(),
        "from": from_ms,
        "to": to_ms,
        "fromIso": iso_ms(from_ms),
        "toIso": iso_ms(to_ms),
        "exportedAt": iso_ms(util::now_ms()),
        "events": new_events.len(),
        "blobs": copied,
        "bytes": bytes,
    });
    util::write_atomic(
        &out.join("export.json"),
        serde_json::to_string_pretty(&export_meta).unwrap_or_default().as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let readme = "Project Life export. Recovery without the program: see README_RECOVERY.txt or run\npython3 recover.py --help\nFormat: blobs/<hh>/<hh>/<sha256>, events/<YYYY-MM>.jsonl\n";
    util::write_atomic(&out.join("README_RECOVERY.txt"), readme.as_bytes()).map_err(|e| e.to_string())?;
    let manifest = out.join("MANIFEST.sha256");
    let manifest_text = build_manifest(out)?;
    util::write_atomic(&manifest, manifest_text.as_bytes()).map_err(|e| e.to_string())?;
    let _ = archive;

    // Verify: re-read every blob, compare with its name, and count against the export journal.
    let mut verified = true;
    let mut problems = Vec::new();
    for h in &refs {
        match fs::read(store::blob_path(&blobs_out, h)) {
            Ok(d) => {
                if store::sha256_bytes(&d) != *h {
                    verified = false;
                    problems.push(h.clone());
                }
            }
            Err(_) => {
                verified = false;
                problems.push(h.clone());
            }
        }
    }
    if !verified {
        util::write_atomic(
            &out.join("EXPORT_FAILED.txt"),
            format!("Export verification failed. Affected blobs: {}\n", problems.join(", ")).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(ExportReport { out: out.to_path_buf(), files: new_events.len(), blobs: copied, bytes, verified, manifest })
}

fn build_manifest(root: &Path) -> Result<String, String> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = fs::read_dir(&d).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let p = e.path();
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if rel == "MANIFEST.sha256" {
                continue;
            }
            let data = fs::read(&p).map_err(|e| e.to_string())?;
            rows.push((rel, store::sha256_bytes(&data)));
        }
    }
    rows.sort();
    let mut out = String::new();
    for (rel, h) in rows {
        out.push_str(&format!("{h}  {rel}\n"));
    }
    Ok(out)
}

pub fn verify_manifest(root: &Path) -> Result<(usize, Vec<String>), String> {
    let text = fs::read_to_string(root.join("MANIFEST.sha256")).map_err(|e| format!("no manifest: {e}"))?;
    let mut ok = 0usize;
    let mut bad = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(2, "  ");
        let (h, rel) = match (it.next(), it.next()) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        match fs::read(root.join(rel)) {
            Ok(d) if store::sha256_bytes(&d) == h => ok += 1,
            Ok(_) => bad.push(format!("{rel}: sha256 mismatch")),
            Err(e) => bad.push(format!("{rel}: {e}")),
        }
    }
    Ok((ok, bad))
}

#[derive(Debug)]
pub struct ImportReport {
    pub project: String,
    pub blobs_added: usize,
    pub events_added: usize,
    pub skipped_blobs: usize,
}

/// Import an export into a project (existing or new). Never modifies or deletes existing data.
pub fn import(
    archive: &Archive,
    export_dir: &Path,
    into: Option<&str>,
    new_name: Option<&str>,
) -> Result<ImportReport, String> {
    let meta_text = fs::read_to_string(export_dir.join("export.json"))
        .map_err(|e| format!("{}: {e}", export_dir.join("export.json").display()))?;
    let meta: Value = serde_json::from_str(&meta_text).map_err(|e| e.to_string())?;
    let src_project_root = meta.get("projectRoot").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let src_name = meta.get("projectName").and_then(|v| v.as_str()).unwrap_or("imported").to_string();
    let src_profile = meta.get("profile").and_then(|v| v.as_str()).unwrap_or("source").to_string();
    let src_settings = meta.get("settings").and_then(|v| v.as_object()).cloned().unwrap_or_default();

    let mut project = match into {
        Some(name) => archive.find(name)?,
        None => {
            let name = new_name.unwrap_or(&src_name).to_string();
            let dir = archive.projects_dir().join(util::uuid_v4());
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let now = iso_ms(util::now_ms());
            let mut obj = Map::new();
            obj.insert("schemaVersion".into(), Value::from(1));
            obj.insert(
                "projectId".into(),
                Value::from(dir.file_name().unwrap_or_default().to_string_lossy().to_string()),
            );
            obj.insert("name".into(), Value::from(name.clone()));
            obj.insert("projectRoot".into(), Value::from(src_project_root.clone()));
            obj.insert("createdAt".into(), Value::from(now.clone()));
            obj.insert("historyStartsAt".into(), Value::from(now));
            obj.insert("profile".into(), Value::from(src_profile));
            obj.insert("settings".into(), Value::Object(src_settings));
            obj.insert("state".into(), Value::from("active"));
            obj.insert("lastSeq".into(), Value::from(0));
            obj.insert("importedFrom".into(), Value::from(export_dir.to_string_lossy().to_string()));
            util::write_atomic(
                &dir.join("project.json"),
                serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default().as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            Project::from_dir(&dir)?
        }
    };

    // FR-LIF-3: an interrupted prune on the target is resolved before anything is written to it.
    if into.is_some() {
        let _ = recover_prune(archive, &mut project);
    }

    // FR-D: history that arrives with older timestamps but newer sequence numbers would be applied
    // last by `state_at()`, so an old `put` could resurrect a file deleted after the export range.
    // v1.0 refuses that instead of merging it badly; a separate project is the safe shape.
    if let Some(name) = into {
        if let Ok(j) = events::load_journal(&project.dir) {
            if !j.events.is_empty() {
                return Err(format!(
                    "import --into is refused: {name} already has {} events in its journal. Imported \
                     events would be appended after them with new sequence numbers while carrying older \
                     timestamps, and state_at() applies events in sequence order — an old `put` would then \
                     be applied last and could resurrect a file deleted since the export range. Nothing was \
                     written and the current state is unchanged.\n\
                     Use `projectlife import {export} --new <name>` to import into its own project. \
                     A chronological merge is planned for v1.1 (SPEC §15.2).",
                    j.events.len(),
                    export = export_dir.display()
                ));
            }
        }
    }

    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut blobs_added = 0usize;
    let mut skipped = 0usize;
    let blobs_dir = export_dir.join("blobs");
    let mut stack = vec![blobs_dir.clone()];
    while let Some(d) = stack.pop() {
        let rd = match fs::read_dir(&d) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let h = match p.file_name().and_then(|s| s.to_str()) {
                Some(s) if store::is_sha256_hex(s) => s.to_string(),
                _ => continue,
            };
            let data = fs::read(&p).map_err(|e| e.to_string())?;
            if store::sha256_bytes(&data) != h {
                return Err(format!("import: blob {h} is corrupted in the export"));
            }
            if store.put(&h, &data)? {
                blobs_added += 1;
            } else {
                skipped += 1;
            }
        }
    }

    let journal = events::load_journal(&project.dir)?;
    let mut next = events::next_seq(&journal.events);
    let mut imported: Vec<Ev> = Vec::new();
    let events_dir = export_dir.join("events");
    if events_dir.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(&events_dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            let text = fs::read_to_string(&f).map_err(|e| e.to_string())?;
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(line) {
                    m.insert("seq".into(), Value::from(next));
                    m.insert("imported".into(), Value::from(true));
                    next += 1;
                    imported.push(m);
                }
            }
        }
    }
    let added = imported.len();
    if !imported.is_empty() {
        let dir = project.events_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in &imported {
            by_month
                .entry(util::month_name(events::ts_of(e)))
                .or_default()
                .push(serde_json::to_string(&Value::Object(e.clone())).map_err(|e| e.to_string())?);
        }
        for (m, lines) in by_month {
            let p = dir.join(format!("{m}.jsonl"));
            let mut body = String::new();
            for l in lines {
                body.push_str(&l);
                body.push('\n');
            }
            use std::io::Write;
            let mut f = fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| e.to_string())?;
            f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        let mut mark = events::ev_new(0, util::now_ms(), "import");
        events::put_str(&mut mark, "from", &export_dir.to_string_lossy());
        events::put_u64(&mut mark, "events", added as u64);
        events::put_u64(&mut mark, "blobs", blobs_added as u64);
        let mut v = vec![mark];
        events::append(&project, &mut v)?;
    }
    project.set_meta("lastSeq", Value::from(next - 1));
    project.save_meta()?;
    let _ = archive;
    Ok(ImportReport { project: project.name.clone(), blobs_added, events_added: added, skipped_blobs: skipped })
}

pub fn archive_delete(archive: &Archive, project: &Project) -> Result<u64, String> {
    let size = project.size_on_disk();
    fs::remove_dir_all(&project.dir).map_err(|e| format!("{}: {e}", project.dir.display()))?;
    archive.log(&format!("archive-delete {}: removed {}", project.name, util::human_size(size)));
    Ok(size)
}

pub fn versions_count(project: &Project) -> Result<usize, String> {
    let j = events::load_journal(&project.dir)?;
    Ok(j.events.iter().filter(|e| events::event_type(e) == "put").count())
}
