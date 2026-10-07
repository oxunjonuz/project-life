//! Retention policy — thinning of old versions, run only by an explicit user command.
//!
//! Two rules shape this module:
//!
//! * FR-LIF-6: automatic cleanup is forbidden. There is no timer, no scheduler and no call site
//!   inside the observation cycle here. The only entry points are `prune --policy` and
//!   `retention`, both typed by a human. A *stored* policy is a note to the human, not an
//!   instruction to the program: `settings.retentionPolicy` is never read by the daemon.
//! * The moments a policy keeps must stay restorable *exactly*. A thinned bucket is therefore not
//!   "the latest event of that bucket" but a **materialised anchor**: a snapshot plus the full file
//!   list at that moment, the same shape `prune` writes for its boundary. The state between two
//!   retained moments is the last retained moment carried forward, and the CLI says so.

use crate::archive::{iso_ms, Archive, Project};
use crate::events::{self, Ev};
use crate::store::Store;
use crate::util;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// `project.json` → `settings.retentionPolicy` — the string the user stored.
pub const SETTING_KEY: &str = "retentionPolicy";
/// `project.json` → `settings.retentionAppliedAt`.
pub const APPLIED_KEY: &str = "retentionAppliedAt";

const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    All,
    PerDay,
    PerWeek,
    PerMonth,
}

impl Unit {
    pub fn label(&self) -> &'static str {
        match self {
            Unit::All => "all",
            Unit::PerDay => "1/day",
            Unit::PerWeek => "1/week",
            Unit::PerMonth => "1/month",
        }
    }

    /// Which bucket a moment belongs to for this unit. Days are local days (the same basis the
    /// journal files use), weeks are whole 7-day blocks from the epoch, months are local calendar
    /// months — all three are named here so the plan can print them.
    fn bucket(&self, ts: i64) -> i64 {
        match self {
            Unit::All => 0,
            Unit::PerDay => {
                let (y, m, d) = util::local_day(ts);
                (y as i64) * 10_000 + (m as i64) * 100 + d as i64
            }
            Unit::PerWeek => ts.div_euclid(7 * DAY_MS),
            Unit::PerMonth => {
                let (y, m, _) = util::local_day(ts);
                (y as i64) * 100 + m as i64
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub days: i64,
    pub unit: Unit,
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub raw: String,
    /// Ascending by `days`.
    pub segments: Vec<Segment>,
}

fn parse_age(s: &str) -> Result<i64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty age in retention policy".into());
    }
    let (num, unit) = match t.chars().last().unwrap() {
        c if c.is_ascii_alphabetic() => (&t[..t.len() - 1], Some(c.to_ascii_lowercase())),
        _ => (t, None),
    };
    let n: i64 = num.trim().parse().map_err(|_| {
        format!("bad age '{s}' in the retention policy: expected a number, optionally with d/w/m/y (for example 7d)")
    })?;
    if n <= 0 {
        return Err(format!("bad age '{s}': must be greater than zero"));
    }
    let days = match unit {
        None | Some('d') => n,
        Some('w') => n * 7,
        Some('m') => n * 30,
        Some('y') => n * 365,
        Some(other) => return Err(format!("unknown age suffix '{other}' in '{s}' (use d, w, m or y)")),
    };
    Ok(days)
}

impl Policy {
    /// `"7d:all,30d:1/day,365d:1/month"` — keep everything younger than 7 days, one version per
    /// day between 7 and 30 days, one per month between 30 and 365 days, nothing older.
    pub fn parse(spec: &str) -> Result<Policy, String> {
        let raw = spec.trim().to_string();
        if raw.is_empty() {
            return Err("empty retention policy".into());
        }
        let mut segments: Vec<Segment> = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (age, unit) = part.split_once(':').ok_or_else(|| {
                format!(
                    "bad retention segment '{part}': expected <age>:<all|1/day|1/week|1/month>, for example 7d:all"
                )
            })?;
            let days = parse_age(age)?;
            let unit = match unit.trim() {
                "all" => Unit::All,
                "1/day" | "day" | "1d" => Unit::PerDay,
                "1/week" | "week" | "1w" => Unit::PerWeek,
                "1/month" | "month" | "1m" => Unit::PerMonth,
                other => {
                    return Err(format!(
                        "unknown retention rule '{other}' in '{part}' (use all, 1/day, 1/week or 1/month)"
                    ))
                }
            };
            segments.push(Segment { days, unit });
        }
        if segments.is_empty() {
            return Err("the retention policy has no segments".into());
        }
        segments.sort_by_key(|s| s.days);
        for w in segments.windows(2) {
            if w[0].days == w[1].days {
                return Err(format!("two retention segments end at the same age ({}d)", w[0].days));
            }
        }
        Ok(Policy { raw, segments })
    }

    pub fn horizon_days(&self) -> i64 {
        self.segments.last().map(|s| s.days).unwrap_or(0)
    }

    /// Which segment a version of this age falls into (the first window that is wide enough).
    fn segment_for_age_days(&self, age_days: i64) -> Option<(usize, Segment)> {
        self.segments
            .iter()
            .enumerate()
            .find(|(_, s)| age_days <= s.days)
            .map(|(i, s)| (i, *s))
    }

    pub fn to_json(&self) -> Value {
        Value::Array(
            self.segments
                .iter()
                .map(|s| serde_json::json!({"withinDays": s.days, "keep": s.unit.label()}))
                .collect(),
        )
    }
}

/// The stored policy of a project, if the user set one. Reading it never causes any action.
pub fn stored_policy(project: &Project) -> Option<String> {
    project.settings().get(SETTING_KEY).and_then(|v| v.as_str()).map(|s| s.to_string())
}

pub fn applied_at(project: &Project) -> Option<String> {
    project.settings().get(APPLIED_KEY).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Store the policy in `project.json` → `settings` → `retentionPolicy`. This is the only thing that
/// happens when the user sets a policy without asking for a prune: a string is written down, nothing
/// is deleted.
pub fn store_policy(project: &mut Project, policy: &Policy) -> Result<(), String> {
    project.settings_mut().insert(SETTING_KEY.to_string(), Value::from(policy.raw.clone()));
    project.save_meta()
}

pub fn clear_policy(project: &mut Project) -> Result<(), String> {
    let s = project.settings_mut();
    s.remove(SETTING_KEY);
    s.remove(APPLIED_KEY);
    project.save_meta()
}

#[derive(Clone, Debug)]
pub struct WindowStat {
    pub from_days: i64,
    pub to_days: i64,
    pub unit: Unit,
    pub buckets: usize,
    pub kept: usize,
    pub dropped: usize,
}

#[derive(Clone, Debug)]
pub struct RetentionPlan {
    pub policy: Policy,
    pub now: i64,
    /// Versions younger than this are kept exactly as they are.
    pub verbatim_from: i64,
    /// Moments that become a materialised anchor (a full state), ascending.
    pub anchors: Vec<i64>,
    pub keep_verbatim_seqs: Vec<u64>,
    pub keep_other_seqs: Vec<u64>,
    pub drop_seqs: Vec<u64>,
    pub versions_before: usize,
    pub versions_kept: usize,
    pub blobs_before: usize,
    pub blobs_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub new_history_starts_at: i64,
    pub windows: Vec<WindowStat>,
    pub oldest_kept: Option<i64>,
    /// The moment where thinning stops: everything at or after it is kept exactly as it is.
    pub boundary_anchor: i64,
}

impl RetentionPlan {
    pub fn versions_dropped(&self) -> usize {
        self.versions_before.saturating_sub(self.versions_kept)
    }
    pub fn bytes_saved(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "policy": self.policy.raw,
            "now": util::fmt_local(self.now),
            "verbatimFrom": util::fmt_local(self.verbatim_from),
            "historyStartsAtAfter": util::fmt_local(self.new_history_starts_at),
            "versionsBefore": self.versions_before,
            "versionsKept": self.versions_kept,
            "versionsDropped": self.versions_dropped(),
            "anchors": self.anchors.iter().map(|t| util::fmt_local(*t)).collect::<Vec<_>>(),
            "blobsBefore": self.blobs_before,
            "blobsAfter": self.blobs_after,
            "bytesBefore": self.bytes_before,
            "bytesAfter": self.bytes_after,
            "windows": self.windows.iter().map(|w| serde_json::json!({
                "fromDays": w.from_days, "toDays": w.to_days, "keep": w.unit.label(),
                "buckets": w.buckets, "kept": w.kept, "dropped": w.dropped,
            })).collect::<Vec<_>>(),
        })
    }
}

fn anchor_block(prev: &BTreeMap<String, events::FileSt>, state: &BTreeMap<String, events::FileSt>, ts: i64, boundary: bool) -> Vec<Ev> {
    let mut out: Vec<Ev> = Vec::new();
    let mut snap = events::ev_new(0, ts, "snapshot");
    events::put_str(&mut snap, "reason", if boundary { "retention-boundary" } else { "retention-anchor" });
    events::put_u64(&mut snap, "files", state.len() as u64);
    out.push(snap);
    // A full state is only full if the things that are gone are said to be gone: without these
    // deletes a file removed between two anchors would reappear after the prune.
    for rel in prev.keys() {
        if !state.contains_key(rel) {
            let mut e = events::ev_new(0, ts, "delete");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "reason", "retention-anchor");
            out.push(e);
        }
    }
    for (rel, f) in state.iter() {
        if f.kind == "symlink" {
            let mut e = events::ev_new(0, ts, "symlink");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "target", f.target.as_deref().unwrap_or(""));
            events::put_u64(&mut e, "mode", f.mode as u64);
            events::put_str(&mut e, "reason", "retention-anchor");
            out.push(e);
        } else if !f.hash.is_empty() {
            let mut e = events::ev_new(0, ts, "put");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "hash", &f.hash);
            events::put_u64(&mut e, "size", f.size);
            events::put_i64(&mut e, "mtime", f.ts);
            events::put_u64(&mut e, "mode", f.mode as u64);
            events::put_str(&mut e, "reason", "retention-anchor");
            out.push(e);
        }
    }
    out
}

fn is_version_kind(t: &str) -> bool {
    matches!(t, "put" | "delete" | "move" | "symlink")
}

/// What the policy would do. Decides everything; `apply` only renders this plan.
pub fn plan(project: &Project, policy: &Policy, now: i64) -> Result<RetentionPlan, String> {
    let journal = events::load_journal(&project.dir)?;
    if journal.events.is_empty() {
        return Err(format!("{}: the journal is empty, there is nothing to prune", project.name));
    }
    let verbatim_from = if matches!(policy.segments[0].unit, Unit::All) {
        now - policy.segments[0].days * DAY_MS
    } else {
        // No "keep everything" window: every version is thinned by its bucket.
        now
    };

    // 1. classify every version event
    let mut winners: BTreeMap<(usize, i64), (i64, u64)> = BTreeMap::new();
    let mut bucket_keys: Vec<BTreeSet<i64>> = vec![BTreeSet::new(); policy.segments.len()];
    let mut per_window: Vec<(usize, usize, usize)> = vec![(0, 0, 0); policy.segments.len()];
    let mut keep_verbatim: Vec<u64> = Vec::new();
    let mut drop_seqs: Vec<u64> = Vec::new();
    let mut version_count = 0usize;
    for e in &journal.events {
        if !is_version_kind(events::event_type(e)) {
            continue;
        }
        version_count += 1;
        let ts = events::ts_of(e);
        let age_days = (now - ts).div_euclid(DAY_MS);
        match policy.segment_for_age_days(age_days) {
            None => drop_seqs.push(events::seq_of(e)),
            Some((idx, seg)) if matches!(seg.unit, Unit::All) => {
                keep_verbatim.push(events::seq_of(e));
                per_window[idx].1 += 1;
            }
            Some((idx, seg)) => {
                let key = seg.unit.bucket(ts);
                bucket_keys[idx].insert(key);
                let slot = winners.entry((idx, key)).or_insert((i64::MIN, 0));
                if (ts, events::seq_of(e)) > *slot {
                    *slot = (ts, events::seq_of(e));
                }
            }
        }
    }
    // the losers of each bucket are dropped; the winners become anchors. The boundary of the
    // "keep everything" window is an anchor too, which is what makes every moment newer than it
    // exact rather than carried forward.
    let mut keep_winner_seqs: BTreeSet<u64> = BTreeSet::new();
    let mut anchors: Vec<i64> = Vec::new();
    for ((idx, _key), (ts, seq)) in winners.iter() {
        keep_winner_seqs.insert(*seq);
        anchors.push(*ts);
        per_window[*idx].1 += 1;
    }
    let boundary_anchor = verbatim_from;
    anchors.push(boundary_anchor);
    anchors.sort_unstable();
    anchors.dedup();
    for e in &journal.events {
        if !is_version_kind(events::event_type(e)) {
            continue;
        }
        let seq = events::seq_of(e);
        if keep_verbatim.contains(&seq) || keep_winner_seqs.contains(&seq) {
            continue;
        }
        drop_seqs.push(seq);
        let age_days = (now - events::ts_of(e)).div_euclid(DAY_MS);
        if let Some((idx, _)) = policy.segment_for_age_days(age_days) {
            per_window[idx].2 += 1;
        }
    }
    for (i, keys) in bucket_keys.iter().enumerate() {
        per_window[i].0 = keys.len();
    }

    // 2. non-version events: marks always survive, the narrative survives as far back as the
    //    oldest retained moment.
    let keep_from = anchors.first().copied().map(|t| t.min(verbatim_from)).unwrap_or(verbatim_from);
    let mut keep_other: Vec<u64> = Vec::new();
    for e in &journal.events {
        if is_version_kind(events::event_type(e)) {
            continue;
        }
        if events::event_type(e) == "mark" || events::ts_of(e) >= keep_from {
            keep_other.push(events::seq_of(e));
        }
    }

    // 3. what survives
    let mut kept_ts: Vec<i64> = anchors.clone();
    for e in &journal.events {
        if keep_verbatim.contains(&events::seq_of(e)) {
            kept_ts.push(events::ts_of(e));
        }
    }
    kept_ts.sort_unstable();
    let oldest_kept = kept_ts.first().copied();
    // A policy that keeps nothing at all from this history is refused, not executed: it would
    // leave one moment (the current state) restorable and destroy the entire past. The boundary
    // anchor always exists, so this is the only condition worth checking and it is the dangerous one.
    let kept_count: usize = per_window.iter().map(|(_, k, _)| *k).sum();
    if kept_count == 0 {
        let newest = journal
            .events
            .iter()
            .filter(|e| is_version_kind(events::event_type(e)))
            .map(events::ts_of)
            .max()
            .unwrap_or(now);
        return Err(format!(
            "policy \"{}\" keeps nothing from this history: the newest version is {} old and the widest \
             window of the policy is {} days, so every version would be dropped and only the current state \
             would stay restorable; nothing was changed.\n\
             Use a wider policy, or `prune --before <date>` if you really mean to cut the history short.",
            policy.raw,
            util::fmt_duration((now - newest).max(0)),
            policy.horizon_days()
        ));
    }
    let new_history_starts_at = oldest_kept.unwrap_or(boundary_anchor);

    // 4. blob accounting
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let all = store.list_all();
    let bytes_before: u64 = all.iter().map(|(_, s)| *s).sum();
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for e in &journal.events {
        if keep_verbatim.contains(&events::seq_of(e)) && events::event_type(e) == "put" {
            if let Some(h) = events::get_str(e, "hash") {
                refs.insert(h);
            }
        }
    }
    for ts in anchors.iter() {
        for f in events::state_at(&journal.events, *ts, None).values() {
            if !f.hash.is_empty() {
                refs.insert(f.hash.clone());
            }
        }
    }
    let blobs_after = all.iter().filter(|(h, _)| refs.contains(h)).count();
    let bytes_after: u64 = all.iter().filter(|(h, _)| refs.contains(h)).map(|(_, s)| *s).sum();

    let windows: Vec<WindowStat> = policy
        .segments
        .iter()
        .enumerate()
        .map(|(i, s)| WindowStat {
            from_days: if i == 0 { 0 } else { policy.segments[i - 1].days },
            to_days: s.days,
            unit: s.unit,
            buckets: per_window[i].0,
            kept: per_window[i].1,
            dropped: per_window[i].2,
        })
        .collect();

    Ok(RetentionPlan {
        policy: policy.clone(),
        now,
        verbatim_from,
        anchors,
        keep_verbatim_seqs: keep_verbatim,
        keep_other_seqs: keep_other,
        drop_seqs,
        versions_before: version_count,
        versions_kept: per_window.iter().map(|(_, k, _)| *k).sum(),
        blobs_before: all.len(),
        blobs_after,
        bytes_before,
        bytes_after,
        new_history_starts_at,
        windows,
        oldest_kept,
        boundary_anchor,
    })
}

fn build_journal(project: &Project, plan: &RetentionPlan) -> Result<Vec<Ev>, String> {
    let journal = events::load_journal(&project.dir)?;
    let mut out: Vec<Ev> = Vec::new();
    let mut prev: BTreeMap<String, events::FileSt> = BTreeMap::new();
    for ts in plan.anchors.iter() {
        let st = events::state_at(&journal.events, *ts, None);
        out.extend(anchor_block(&prev, &st, *ts, *ts == plan.verbatim_from));
        prev = st;
    }
    let keep_verbatim: BTreeSet<u64> = plan.keep_verbatim_seqs.iter().copied().collect();
    let keep_other: BTreeSet<u64> = plan.keep_other_seqs.iter().copied().collect();
    for e in &journal.events {
        let seq = events::seq_of(e);
        if keep_verbatim.contains(&seq) || keep_other.contains(&seq) {
            out.push(e.clone());
        }
    }
    // stable order by time: anchors carry the moments, everything else keeps its own stamp
    out.sort_by_key(events::ts_of);
    for (i, e) in out.iter_mut().enumerate() {
        e.insert("seq".into(), Value::from((i + 1) as u64));
    }
    Ok(out)
}

pub struct Outcome {
    pub plan: RetentionPlan,
    pub removed_blobs: usize,
    pub journal_events_after: usize,
}

/// Apply the policy. This is a user command and nothing else: no path in the program reaches it
/// except `prune --policy` and `retention apply`.
pub fn apply(archive: &Archive, project: &mut Project, policy: &Policy, now: i64) -> Result<Outcome, String> {
    let plan = plan(project, policy, now)?;
    let events = build_journal(project, &plan)?;
    let journal_events_after = events.len();
    let removed = crate::lifecycle::swap_journal(
        archive,
        project,
        events,
        plan.new_history_starts_at,
        &format!("retention-policy {}", project.name),
    )?;
    store_policy(project, policy)?;
    project.settings_mut().insert(APPLIED_KEY.to_string(), Value::from(iso_ms(now)));
    project.save_meta()?;
    archive.log(&format!(
        "retention {}: policy \"{}\" applied — {} of {} versions dropped, {} moments kept as anchors, {} blobs deleted",
        project.name,
        policy.raw,
        plan.versions_dropped(),
        plan.versions_before,
        plan.anchors.len(),
        removed
    ));
    crate::ops::record(
        archive,
        "retention",
        &project.name,
        serde_json::json!({
            "policy": policy.raw,
            "versionsBefore": plan.versions_before,
            "versionsAfter": plan.versions_kept,
            "versionsDropped": plan.versions_dropped(),
            "anchors": plan.anchors.len(),
            "removedBlobs": removed,
        }),
    );
    Ok(Outcome { plan, removed_blobs: removed, journal_events_after })
}

/// Was this history thinned by a retention policy? Returns the newest anchor moment, if any.
/// The restore path asks this so it can say "the state between retained moments is the newer one
/// carried forward" at the moment it matters, not only when the policy was applied.
pub fn thinned_at(project: &Project) -> Result<Option<i64>, String> {
    let journal = events::load_journal(&project.dir)?;
    Ok(journal
        .events
        .iter()
        .filter(|e| {
            events::event_type(e) == "snapshot"
                && matches!(
                    events::get_str(e, "reason").as_deref(),
                    Some("retention-anchor") | Some("retention-boundary")
                )
        })
        .map(events::ts_of)
        .max())
}

/// Blobs on disk that no journal event references — reported, never deleted here.
pub fn dangling(project: &Project) -> Result<(usize, u64, Vec<String>), String> {
    let journal = events::load_journal(&project.dir)?;
    let refs: BTreeSet<String> = journal
        .events
        .iter()
        .filter(|e| events::event_type(e) == "put")
        .filter_map(|e| events::get_str(e, "hash"))
        .collect();
    let store = Store::new(&project.blobs_dir(), &project.tmp_dir());
    let mut n = 0usize;
    let mut bytes = 0u64;
    let mut names: Vec<String> = Vec::new();
    for (h, sz) in store.list_all() {
        if !refs.contains(&h) {
            n += 1;
            bytes += sz;
            names.push(h);
        }
    }
    Ok((n, bytes, names))
}
