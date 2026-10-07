//! The event journal: the single source of truth. Append-only JSON Lines, one file per month.
//!
//! Integrity rules:
//!  * one line is one event; a half-written last line is ignored, corruption in the middle is an
//!    error (the project goes to `error` and nothing is deleted);
//!  * the state at moment T is obtained by applying events with `ts <= T` in ascending `seq`.

use crate::archive::{iso_ms, Project};
use crate::util;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Ev = Map<String, Value>;

/// Bytes read by reading journal files in full. A per-round measurement, printed in the pass report:
/// a partial pass must leave this at zero, and that is what makes "it does not read the journal" a
/// number rather than a promise.
pub static JOURNAL_FULL_BYTES_READ: AtomicU64 = AtomicU64::new(0);
/// Bytes read by the tail probe: the last line of the newest journal file, not the file.
pub static JOURNAL_TAIL_BYTES_READ: AtomicU64 = AtomicU64::new(0);

pub struct Journal {
    pub events: Vec<Ev>,
    pub trailing_partial: bool,
}

pub fn ev_new(seq: u64, ts: i64, etype: &str) -> Ev {
    let mut m = Map::new();
    m.insert("seq".into(), Value::from(seq));
    m.insert("ts".into(), Value::from(ts));
    m.insert("type".into(), Value::from(etype));
    m
}

pub fn put_str(e: &mut Ev, k: &str, v: &str) {
    e.insert(k.to_string(), Value::from(v));
}
pub fn put_u64(e: &mut Ev, k: &str, v: u64) {
    e.insert(k.to_string(), Value::from(v));
}
pub fn put_i64(e: &mut Ev, k: &str, v: i64) {
    e.insert(k.to_string(), Value::from(v));
}

pub fn get_str(e: &Ev, k: &str) -> Option<String> {
    e.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}
pub fn get_u64(e: &Ev, k: &str) -> Option<u64> {
    e.get(k).and_then(|v| v.as_u64())
}
pub fn get_i64(e: &Ev, k: &str) -> Option<i64> {
    e.get(k).and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
}
pub fn get_f64(e: &Ev, k: &str) -> Option<f64> {
    e.get(k).and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
}

pub fn event_type(e: &Ev) -> &str {
    e.get("type").and_then(|v| v.as_str()).unwrap_or("")
}

pub fn seq_of(e: &Ev) -> u64 {
    e.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
}

pub fn ts_of(e: &Ev) -> i64 {
    e.get("ts").and_then(|v| v.as_i64()).unwrap_or(0)
}

/// Every event of the project, sorted by `seq`. `Err` means the journal is corrupted in the middle.
pub fn load_journal(project_dir: &Path) -> Result<Journal, String> {
    let dir = project_dir.join("events");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if dir.is_dir() {
        for e in util::read_dir_sorted(&dir).map_err(|e| e.to_string())? {
            if e.extension().map(|x| x == "jsonl").unwrap_or(false) {
                files.push(e);
            }
        }
    }
    let mut events: Vec<Ev> = Vec::new();
    let mut trailing_partial = false;
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        JOURNAL_FULL_BYTES_READ.fetch_add(text.len() as u64, Ordering::Relaxed);
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(l) {
                Ok(Value::Object(m)) => events.push(m),
                Ok(_) => return Err(format!("{}: line is not an object: {}", f.display(), &l[..l.len().min(120)])),
                Err(e) => {
                    let is_last = lines.peek().is_none();
                    if is_last && !l.ends_with('}') {
                        // a half-written trailing line after a crash is normal
                        trailing_partial = true;
                        continue;
                    }
                    return Err(format!(
                        "journal corrupted in the middle: {}: {e}\nline: {}",
                        f.display(),
                        &l[..l.len().min(160)]
                    ));
                }
            }
        }
    }
    events.sort_by_key(seq_of);
    Ok(Journal { events, trailing_partial })
}

pub fn next_seq(events: &[Ev]) -> u64 {
    events.iter().map(seq_of).max().unwrap_or(0) + 1
}

pub fn last_ts(events: &[Ev]) -> Option<i64> {
    events.iter().map(ts_of).max()
}

pub fn first_ts(events: &[Ev]) -> Option<i64> {
    events.iter().map(ts_of).min()
}

/// Append events: `seq` is assigned here, the file is chosen by the month of `ts`.
/// By this point every referenced blob is already on disk (invariant FR-STO-5).
///
/// Round 294: the next sequence number comes from the journal's tail (`cache/tail.json`), validated
/// against the last line of the newest journal file — a few hundred bytes instead of the whole
/// journal. If the tail cannot be trusted (missing, or disagreeing with the file), the journal is
/// read in full: the slow path is the correct path, and it repairs the tail.
pub fn append(project: &Project, evs: &mut [Ev]) -> Result<u64, String> {
    let (mut seq, prev_ts, mut observed) = match tail(project) {
        Ok(t) => (t.seq_last + 1, t.ts_last, t.observed_at),
        Err(_) => {
            let journal = load_journal(&project.dir)?;
            (
                next_seq(&journal.events),
                last_ts(&journal.events).unwrap_or(0),
                last_observed_at(&journal.events),
            )
        }
    };
    let dir = project.events_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in evs.iter_mut() {
        e.insert("seq".into(), Value::from(seq));
        seq += 1;
        let ts = ts_of(e);
        if is_observation(event_type(e)) {
            observed = Some(observed.map(|o| o.max(ts)).unwrap_or(ts));
        }
        let month = util::month_name(ts);
        let line = serde_json::to_string(&Value::Object(e.clone())).map_err(|e| e.to_string())?;
        by_month.entry(month).or_default().push(line);
    }
    let last_seq = seq - 1;
    let mut last_ts = prev_ts;
    for e in evs.iter() {
        last_ts = last_ts.max(ts_of(e));
    }
    for (month, lines) in by_month {
        let path = dir.join(format!("{month}.jsonl"));
        let mut payload = String::new();
        for l in &lines {
            payload.push_str(l);
            payload.push('\n');
        }
        // Never join a record to a half-written last line: a crash can leave the file ending without
        // a newline, and the two records would then become one unreadable line.
        crate::cache::append_lines(&path, &payload)?;
    }
    write_tail(
        &project.dir,
        &Tail { version: 1, seq_last: last_seq, ts_last: last_ts, observed_at: observed, source: "append".into() },
    )?;
    Ok(last_seq)
}

/// Event types that count as "the project was observed" — the same set the interval report used.
pub fn is_observation(t: &str) -> bool {
    matches!(t, "put" | "delete" | "move" | "symlink" | "snapshot" | "mass" | "skip")
}

// -------------------------------------------------------------------------------------------
// The journal tail: seq/ts of the last event, without reading the journal.
// -------------------------------------------------------------------------------------------

/// The journal's fast index, in `cache/tail.json`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Tail {
    #[serde(rename = "version", default)]
    pub version: u64,
    #[serde(rename = "seqLast", default)]
    pub seq_last: u64,
    #[serde(rename = "tsLast", default)]
    pub ts_last: i64,
    #[serde(rename = "observedAt", default)]
    pub observed_at: Option<i64>,
    #[serde(default)]
    pub source: String,
}

/// The tail lives in `cache/`, not in `events/`: the journal directory holds the journal and
/// nothing else, so any reader that walks it — including `tools/recover.py` and a stranger with
/// `cat` — sees events and only events. (The first version of this round put it in `events/`; the
/// verification scripts that read "every file in events/" broke on it immediately.)
pub fn tail_path(project_dir: &Path) -> PathBuf {
    project_dir.join("cache").join("tail.json")
}

/// What the newest journal file says: `(seq, ts)` of its last complete line.
///
/// The newest file is the one with the newest mtime, and that is not a heuristic: `append` writes
/// sequentially, so the file it last wrote to is the file whose last line carries the highest
/// sequence number. Reading its last line is a few hundred bytes and it is the evidence the tail is
/// checked against. Several files may share an mtime (a clock jump inside one append); then the
/// highest sequence among them is taken.
pub fn probe_journal(project_dir: &Path) -> Result<(u64, i64), String> {
    let dir = project_dir.join("events");
    let mut files: Vec<(i64, PathBuf)> = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() || p.extension().map(|x| x == "jsonl").unwrap_or(false) == false {
            continue;
        }
        if let Ok(md) = e.metadata() {
            files.push((util::ms_of(md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)), p));
        }
    }
    if files.is_empty() {
        return Ok((0, 0));
    }
    let newest = files.iter().map(|(m, _)| *m).max().unwrap_or(0);
    let mut best: Option<(u64, i64)> = None;
    for (m, p) in files.iter().filter(|(m, _)| *m == newest) {
        let _ = m;
        if let Some((s, t)) = last_line_of(p)? {
            best = Some(match best {
                Some(b) if b.0 >= s => b,
                _ => (s, t),
            });
        }
    }
    best.ok_or_else(|| "no complete event line in the newest journal file".to_string())
}

/// The last complete line of one journal file, as `(seq, ts)`.
fn last_line_of(path: &Path) -> Result<Option<(u64, i64)>, String> {
    // The last event line is a few hundred bytes; a `mass` event with its sample paths can be ~10 KB,
    // so the window is widened once before giving up (a full read of the journal follows then).
    for block in [4096u64, 65536] {
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        if len == 0 {
            return Ok(None);
        }
        let from = len.saturating_sub(block);
        let mut f = f;
        f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        JOURNAL_TAIL_BYTES_READ.fetch_add(buf.len() as u64, Ordering::Relaxed);
        let text = String::from_utf8_lossy(&buf);
        let mut first = true;
        for line in text.lines().rev() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            // The very last line may be half-written after a crash: then its predecessor is the answer.
            match serde_json::from_str::<Value>(l) {
                Ok(v) => {
                    let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
                    let ts = v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0);
                    return Ok(Some((seq, ts)));
                }
                Err(_) if first && from > 0 => {
                    first = false;
                    continue;
                }
                Err(_) => break,
            }
        }
        if from == 0 {
            break;
        }
    }
    Ok(None)
}

/// The tail, validated. `Err` means "read the journal in full"; the caller then writes a fresh tail.
pub fn probe_tail(project_dir: &Path) -> Result<Tail, String> {
    let text = std::fs::read_to_string(tail_path(project_dir)).map_err(|e| format!("no tail: {e}"))?;
    let t: Tail = serde_json::from_str(&text).map_err(|e| format!("tail is unreadable: {e}"))?;
    let (seq, ts) = probe_journal(project_dir)?;
    if t.seq_last != seq {
        return Err(format!("the tail says seq {} but the journal ends at {}", t.seq_last, seq));
    }
    if t.ts_last != ts {
        return Err(format!("the tail says ts {} but the journal ends at {}", t.ts_last, ts));
    }
    Ok(t)
}

pub fn write_tail(project_dir: &Path, t: &Tail) -> Result<(), String> {
    // Never create the journal directory here. It is missing exactly in one interesting moment —
    // between the two renames of a prune — and a reader that creates it makes the recovery path
    // believe the journal is already in place, which would lose the swapped-out history. A tail is
    // an index of a journal; with no journal there is nothing to index.
    if !project_dir.join("events").is_dir() {
        return Err("no journal directory: nothing to index".into());
    }
    let dir = project_dir.join("cache");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut t = t.clone();
    t.version = 1;
    let text = serde_json::to_string(&t).map_err(|e| e.to_string())?;
    // Derived and rebuilt on the next run: no fsync (see util::write_atomic_lazy).
    util::write_atomic_lazy(&tail_path(project_dir), text.as_bytes()).map_err(|e| e.to_string())
}

/// Forget the tail (after the journal was rewritten wholesale, e.g. by a prune): the next read
/// rebuilds it from the journal.
pub fn invalidate_tail(project_dir: &Path) {
    let _ = std::fs::remove_file(tail_path(project_dir));
}

/// The tail, or a fresh one rebuilt from a full read of the journal.
pub fn tail(project: &Project) -> Result<Tail, String> {
    if let Ok(t) = probe_tail(&project.dir) {
        return Ok(t);
    }
    // No journal directory at all (between the two renames of a prune): there is nothing to index,
    // and creating the directory here would hide that window from the recovery path.
    if !project.events_dir().is_dir() {
        return Ok(Tail { version: 1, seq_last: 0, ts_last: 0, observed_at: None, source: "no-journal".into() });
    }
    let journal = load_journal(&project.dir)?;
    let t = Tail {
        version: 1,
        seq_last: next_seq(&journal.events).saturating_sub(1),
        ts_last: last_ts(&journal.events).unwrap_or(0),
        observed_at: last_observed_at(&journal.events),
        source: "rebuilt".into(),
    };
    write_tail(&project.dir, &t)?;
    Ok(t)
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSt {
    pub hash: String,
    pub size: u64,
    pub mode: u32,
    pub kind: String, // "file" | "symlink"
    pub target: Option<String>,
    pub seq: u64,
    pub ts: i64,
}

/// The project state at moment T. `upto_seq` (if given) limits by sequence number.
pub fn state_at(events: &[Ev], t_ms: i64, upto_seq: Option<u64>) -> BTreeMap<String, FileSt> {
    let mut st: BTreeMap<String, FileSt> = BTreeMap::new();
    for e in events {
        let seq = seq_of(e);
        if let Some(lim) = upto_seq {
            if seq > lim {
                continue;
            }
        }
        if ts_of(e) > t_ms {
            continue;
        }
        match event_type(e) {
            "put" => {
                if let Some(p) = get_str(e, "path") {
                    st.insert(
                        p,
                        FileSt {
                            hash: get_str(e, "hash").unwrap_or_default(),
                            size: get_u64(e, "size").unwrap_or(0),
                            mode: get_u64(e, "mode").unwrap_or(0o644) as u32,
                            kind: "file".into(),
                            target: None,
                            seq,
                            ts: ts_of(e),
                        },
                    );
                }
            }
            "symlink" => {
                if let Some(p) = get_str(e, "path") {
                    st.insert(
                        p,
                        FileSt {
                            hash: String::new(),
                            size: 0,
                            mode: get_u64(e, "mode").unwrap_or(0o777) as u32,
                            kind: "symlink".into(),
                            target: get_str(e, "target"),
                            seq,
                            ts: ts_of(e),
                        },
                    );
                }
            }
            "delete" => {
                if let Some(p) = get_str(e, "path") {
                    st.remove(&p);
                }
            }
            "move" => {
                let from = get_str(e, "from");
                let to = get_str(e, "to");
                if let (Some(f), Some(t)) = (from, to) {
                    if let Some(mut v) = st.remove(&f) {
                        v.seq = seq;
                        v.ts = ts_of(e);
                        st.insert(t, v);
                    }
                }
            }
            _ => {}
        }
    }
    st
}

/// The moment of the last observation (any event that represents looking at the disk).
pub fn last_observed_at(events: &[Ev]) -> Option<i64> {
    events
        .iter()
        .filter(|e| {
            matches!(
                event_type(e),
                "put" | "delete" | "move" | "symlink" | "snapshot" | "mass" | "skip"
            )
        })
        .map(ts_of)
        .max()
}

/// Observation gaps recorded in the journal.
pub fn gaps(events: &[Ev]) -> Vec<(i64, i64, String)> {
    events
        .iter()
        .filter(|e| event_type(e) == "gap")
        .map(|e| {
            (
                get_i64(e, "from").unwrap_or(0),
                get_i64(e, "to").unwrap_or(0),
                get_str(e, "reason").unwrap_or_default(),
            )
        })
        .collect()
}

pub fn iso(ms: i64) -> String {
    iso_ms(ms)
}
