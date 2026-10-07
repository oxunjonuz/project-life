//! The state cache: what the last observation saw, in a form a partial pass can read cheaply.
//!
//! Round 294. The cache used to be one JSON object — every pass parsed all of it and wrote all of it
//! back. On a project of 10 000 files that was ~1.9 MB read and ~1.9 MB written for a pass that
//! changed one file, and it was the whole of a partial pass's bookkeeping cost.
//!
//! The cache is now two files:
//!
//! * `cache/base.jsonl` — one JSON record per tracked path, **sorted by path**. Written only by a
//!   full pass (and by a compaction or a rebuild). Because it is sorted, one record is found by a
//!   binary search over byte offsets and one directory's subtree by a seek and a forward read, so a
//!   partial pass reads a few kilobytes instead of the file. `cache/base.meta.json` next to it
//!   carries the entry count so the count is not a scan.
//! * `cache/delta.jsonl` — append-only records written since the last base write, in order. The last
//!   record for a path wins; `"op":"del"` is a tombstone ("no longer tracked").
//!
//! Folding the delta into a new base **is idempotent** — applying the same records again to a state
//! that already contains them changes nothing — so a crash between the base write and the delta
//! reset needs no swap journal: the next reader applies the delta again and gets the same state.
//!
//! The cache is derived data. It can always be rebuilt from the journal (`pl rebuild-cache`), a
//! damaged base is not fatal, and a pass that finds no cache falls back to rebuilding it from the
//! journal exactly as the old code did. The journal is the truth; this is the index that makes
//! reading it unnecessary.
//!
//! Nothing here decides anything about the project: entries are written by the pass, from what the
//! pass observed.

use crate::archive::{iso_ms, Project};
use crate::util;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The cache format this build writes. 1 was one `state.json` object.
pub const CACHE_FORMAT_VERSION: u64 = 2;

/// How far around a byte a probe looks for the line boundaries. A record is a few hundred bytes;
/// the window only grows for a path long enough not to fit.
const WINDOW: u64 = 512;

/// One tracked path, as last observed.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    /// "file" | "symlink"
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub dev: Option<u64>,
    #[serde(default)]
    pub ino: Option<u64>,
}

impl CacheEntry {
    pub fn file(hash: String, size: u64, mtime: i64, mode: u32, dev: Option<u64>, ino: Option<u64>) -> CacheEntry {
        CacheEntry { hash, size, mtime, mode, kind: "file".into(), target: None, dev, ino }
    }
}

// ---------------------------------------------------------------------------------------------
// A sorted JSON Lines file: `{"path": "...", ...}` per line, ascending by path.
// ---------------------------------------------------------------------------------------------

pub struct SortedFile {
    pub path: PathBuf,
    len: u64,
    file: RefCell<File>,
    /// Bytes this handle actually read. The pass reports it, so "reads a few kilobytes" is a number.
    read: RefCell<u64>,
}

impl SortedFile {
    pub fn open(path: &Path) -> Result<Option<SortedFile>, String> {
        match File::open(path) {
            Ok(f) => {
                let len = f.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len();
                Ok(Some(SortedFile { path: path.to_path_buf(), len, file: RefCell::new(f), read: RefCell::new(0) }))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn bytes_read(&self) -> u64 {
        *self.read.borrow()
    }

    fn read_at(&self, off: u64, n: usize) -> Result<Vec<u8>, String> {
        if off >= self.len || n == 0 {
            return Ok(Vec::new());
        }
        let n = n.min((self.len - off) as usize);
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start(off)).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            let k = f.read(&mut buf[got..]).map_err(|e| format!("{}: {e}", self.path.display()))?;
            if k == 0 {
                break;
            }
            got += k;
        }
        buf.truncate(got);
        *self.read.borrow_mut() += got as u64;
        Ok(buf)
    }

    /// The line that contains byte `off`: (offset of its first byte, the line without the newline).
    ///
    /// One read per probe. A record is a few hundred bytes, so a small window around `off` almost
    /// always holds the whole line, and a binary search over a 2 MB base then costs a few kilobytes
    /// per probe instead of a block in each direction. A line longer than the window is still
    /// handled: the search widens backwards and forwards until it finds the newlines.
    fn line_at(&self, off: u64) -> Result<(u64, String), String> {
        if self.len == 0 {
            return Ok((0, String::new()));
        }
        let probe = off.min(self.len - 1);
        let mut w = WINDOW;
        loop {
            let from = probe.saturating_sub(w);
            let to = (probe + w).min(self.len);
            let buf = self.read_at(from, (to - from) as usize)?;
            let head = (probe - from) as usize;
            let start_back = buf[..head].iter().rposition(|b| *b == b'\n');
            let end_fwd = buf[head.min(buf.len())..].iter().position(|b| *b == b'\n');
            let start = match start_back {
                Some(i) => from + i as u64 + 1,
                None if from == 0 => 0,
                None => {
                    w *= 4;
                    if w > self.len + WINDOW {
                        return Ok((0, String::new()));
                    }
                    continue;
                }
            };
            let end = match end_fwd {
                Some(i) => probe + i as u64,
                None if to >= self.len => self.len,
                None => {
                    w *= 4;
                    continue;
                }
            };
            let line_start = start;
            let line_end = end.max(line_start);
            let line = self.read_at(line_start, (line_end - line_start) as usize)?;
            return Ok((line_start, String::from_utf8_lossy(&line).to_string()));
        }
    }

    fn path_of(line: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("not a JSON record ({e}): {}", &line[..line.len().min(120)]))?;
        v.get("path")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("record without a path: {}", &line[..line.len().min(120)]))
    }

    /// Offset of the first line whose path is `>= target` (0 when everything is smaller).
    fn lower_bound(&self, target: &str) -> Result<u64, String> {
        let (mut lo, mut hi) = (0u64, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (start, line) = self.line_at(mid)?;
            if line.trim().is_empty() {
                lo = mid + 1;
                continue;
            }
            let p = Self::path_of(&line)?;
            if p.as_str() < target {
                lo = start + line.len() as u64 + 1;
            } else {
                hi = start;
            }
        }
        Ok(lo)
    }

    /// The record whose path is exactly `want`.
    pub fn find(&self, want: &str) -> Result<Option<Value>, String> {
        let mut off = self.lower_bound(want)?;
        while off < self.len {
            let (start, line) = self.line_at(off)?;
            let next = (start + line.len() as u64 + 1).max(off + 1);
            if line.trim().is_empty() {
                off = next;
                continue;
            }
            let p = Self::path_of(&line)?;
            if p == want {
                return Ok(Some(serde_json::from_str(&line).map_err(|e| e.to_string())?));
            }
            if p.as_str() > want {
                return Ok(None);
            }
            off = next;
        }
        Ok(None)
    }

    /// Every record whose path starts with `prefix`, in path order.
    pub fn scan_prefix(&self, prefix: &str) -> Result<Vec<(String, Value)>, String> {
        let mut out = Vec::new();
        let mut off = self.lower_bound(prefix)?;
        while off < self.len {
            let (start, line) = self.line_at(off)?;
            let next = (start + line.len() as u64 + 1).max(off + 1);
            if line.trim().is_empty() {
                off = next;
                continue;
            }
            let p = Self::path_of(&line)?;
            if !p.starts_with(prefix) {
                if p.as_str() < prefix {
                    off = next;
                    continue;
                }
                break;
            }
            out.push((p, serde_json::from_str(&line).map_err(|e| e.to_string())?));
            off = next;
        }
        Ok(out)
    }

    /// Every record, in path order. Only a full pass (and a compaction) does this.
    pub fn read_all(&self) -> Result<Vec<(String, Value)>, String> {
        let mut out = Vec::new();
        let mut off = 0u64;
        while off < self.len {
            let (start, line) = self.line_at(off)?;
            let next = (start + line.len() as u64 + 1).max(off + 1);
            if !line.trim().is_empty() {
                let p = Self::path_of(&line)?;
                out.push((p, serde_json::from_str(&line).map_err(|e| e.to_string())?));
            }
            off = next;
        }
        Ok(out)
    }
}

fn entry_of(v: &Value) -> Result<CacheEntry, String> {
    serde_json::from_value::<CacheEntry>(v.clone()).map_err(|e| e.to_string())
}

fn record_line(path: &str, e: &CacheEntry) -> Result<String, String> {
    let mut obj = match serde_json::to_value(e).map_err(|e| e.to_string())? {
        Value::Object(m) => m,
        _ => return Err("a cache entry is not an object".into()),
    };
    obj.insert("path".into(), Value::from(path.to_string()));
    serde_json::to_string(&Value::Object(obj)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// The cache itself: base + delta
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Op {
    Set(CacheEntry),
    Del,
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub path: String,
    pub op: Op,
    pub at: i64,
    /// Was the path tracked when this record was written? The count of tracked paths is derived
    /// from the delta alone with this, instead of one binary search into the base per record —
    /// which on a 1 900-record delta was 6.5 MB of reads for a pass that touched one file.
    pub was: Option<bool>,
}

/// `{"path":...,"at":...,"was":...,<entry>}` — written by hand so that `path` is the **first** key.
/// The reader can then take a record's path out of a line without parsing the line, which is what
/// makes "which records does this pass care about" a byte scan instead of a parse of every record.
fn encode_rec(r: &Rec) -> Result<String, String> {
    let path = serde_json::to_string(&r.path).map_err(|e| e.to_string())?;
    let mut out = format!("{{\"path\":{path},\"at\":{}", r.at);
    if let Some(w) = r.was {
        out.push_str(&format!(",\"was\":{w}"));
    }
    match &r.op {
        Op::Del => out.push_str(",\"op\":\"del\""),
        Op::Set(e) => {
            let v = serde_json::to_string(e).map_err(|e| e.to_string())?;
            // the entry is an object; splice its fields in
            let inner = v.trim_start_matches('{').trim_end_matches('}');
            if !inner.is_empty() {
                out.push(',');
                out.push_str(inner);
            }
        }
    }
    out.push('}');
    Ok(out)
}

/// `"was"` out of a record line without parsing it.
fn raw_was(line: &str) -> Option<bool> {
    let i = line.rfind("\"was\":")?;
    match line[i + 6..].chars().next() {
        Some('t') => Some(true),
        Some('f') => Some(false),
        _ => None,
    }
}

/// The path of a record line, without parsing it. `None` means "not readable this way" — the caller
/// then falls back to a real parse, so a path containing a quote is handled correctly, only slower.
fn raw_line_path(line: &str) -> Option<String> {
    // The prefix includes the opening quote of the value: the scanner starts *inside* the string.
    let rest = line.trim_start().strip_prefix("{\"path\":\"")?;
    let mut out = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let n = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(n)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

fn decode_rec(v: &Value) -> Result<Rec, String> {
    let path = v.get("path").and_then(|p| p.as_str()).ok_or("a delta record without a path")?.to_string();
    let at = v.get("at").and_then(|x| x.as_i64()).unwrap_or(0);
    let op = if v.get("op").and_then(|x| x.as_str()) == Some("del") {
        Op::Del
    } else {
        Op::Set(entry_of(v)?)
    };
    let was = v.get("was").and_then(|x| x.as_bool());
    Ok(Rec { path, op, at, was })
}

/// The delta as it is read: the file's text once, plus one span per record. A record is parsed only
/// when a question is asked about it, so a pass that touches one file does not pay for the whole
/// delta — and the count of tracked paths comes from `cache/tracked.json`, not from scanning it.
pub struct Delta {
    pub path: PathBuf,
    pub bytes: u64,
    text: String,
    spans: Vec<Span>,
    /// The net change these records make to the number of tracked paths. Every record carries the
    /// transition it made (`was`), so this is a sum over the records in order — no base lookups, no
    /// scan of the base, and no sidecar file to keep in step.
    pub net: i64,
    /// Records written by a build that did not store `was`: their transition needs the base.
    pub unwindable: Vec<String>,
    /// A damaged line that is not the last one: the caller falls back to a rebuild from the journal.
    pub damaged: Option<String>,
}

#[derive(Clone, Debug)]
struct Span {
    path: String,
    start: usize,
    end: usize,
}

impl Delta {
    pub fn open(path: &Path) -> Result<Delta, String> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let bytes = text.len() as u64;
        let mut spans: Vec<Span> = Vec::new();
        let mut damaged: Option<String> = None;
        let mut net: i64 = 0;
        let mut unwindable: Vec<String> = Vec::new();
        let mut start = 0usize;
        let mut lines: Vec<(usize, &str)> = Vec::new();
        for line in text.split_inclusive('\n') {
            lines.push((start, line));
            start += line.len();
        }
        let last_index = lines.len().saturating_sub(1);
        for (i, (off, line)) in lines.into_iter().enumerate() {
            let raw = line.trim_end_matches(['\n', '\r']);
            if raw.trim().is_empty() {
                continue;
            }
            let end = off + raw.len();
            match raw_line_path(raw) {
                Some(p) => {
                    let del = raw.contains("\"op\":\"del\"");
                    match raw_was(raw) {
                        Some(w) => match (w, del) {
                            (false, false) => net += 1,
                            (true, true) => net -= 1,
                            _ => {}
                        },
                        None => unwindable.push(p.clone()),
                    }
                    spans.push(Span { path: p, start: off, end })
                }
                None => {
                    // A path this scanner cannot read: parse the line properly to find out.
                    match serde_json::from_str::<Value>(raw) {
                        Ok(v) => match v.get("path").and_then(|x| x.as_str()) {
                            Some(p) => spans.push(Span { path: p.to_string(), start: off, end }),
                            None => damaged = Some("a delta record without a path".into()),
                        },
                        Err(e) => {
                            let is_last = i == last_index && !raw.ends_with('}');
                            if !is_last {
                                damaged = Some(format!("delta damaged in the middle: {e}"));
                            }
                        }
                    }
                }
            }
        }
        Ok(Delta { path: path.to_path_buf(), bytes, text, spans, net, unwindable, damaged })
    }

    fn line(&self, s: &Span) -> &str {
        &self.text[s.start..s.end]
    }

    fn parse(&self, s: &Span) -> Result<Rec, String> {
        let v: Value = serde_json::from_str(self.line(s)).map_err(|e| format!("delta record: {e}"))?;
        decode_rec(&v)
    }

    /// The last record written for this path.
    pub fn last(&self, path: &str) -> Result<Option<Rec>, String> {
        for s in self.spans.iter().rev() {
            if s.path == path {
                return Ok(Some(self.parse(s)?));
            }
        }
        Ok(None)
    }

    /// Are there any records for this path?
    pub fn mentions(&self, path: &str) -> bool {
        self.spans.iter().any(|s| s.path == path)
    }

    /// Every record whose path the scope covers — the only records a partial pass may reason about.
    pub fn covered(&self, scope: &crate::scan::Scope) -> Result<Vec<Rec>, String> {
        let mut out = Vec::new();
        for s in self.spans.iter().filter(|s| scope.covers(&s.path)) {
            out.push(self.parse(s)?);
        }
        Ok(out)
    }

    /// Every record, parsed. Only a compaction (and the fallback count) does this.
    pub fn all(&self) -> Result<Vec<Rec>, String> {
        let mut out = Vec::new();
        for s in &self.spans {
            out.push(self.parse(s)?);
        }
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

/// Append lines to a file that is one record per line (the journal and the cache delta).
///
/// A crash can leave the file ending without a newline, and what is there can be one of two things:
///
///  * a **complete record whose newline never landed** — it is a record, so the newline is added and
///    the record is kept;
///  * a **torn record** — half a line. Closing it would leave a line in the middle of the file that
///    no reader can parse, and the journal has no repair for the middle; joining the next record onto
///    it would do the same. So it is cut off at the last newline. Nothing is lost: a torn line was
///    never a record, and no reader ever counted it as one.
pub fn append_lines(path: &Path, payload: &str) -> Result<(), String> {
    if payload.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let created = len == 0;
    if len > 0 {
        let from = len.saturating_sub(65536);
        f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.last() != Some(&b'\n') {
            let start = buf.iter().rposition(|b| *b == b'\n').map(|i| from + i as u64 + 1).unwrap_or(0);
            let tail = String::from_utf8_lossy(&buf[(start - from) as usize..]).to_string();
            if serde_json::from_str::<Value>(tail.trim()).is_ok() {
                f.write_all(b"\n").map_err(|e| e.to_string())?;
            } else {
                f.set_len(start).map_err(|e| e.to_string())?;
            }
        }
        f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    }
    f.write_all(payload.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    f.sync_all().map_err(|e| format!("{}: {e}", path.display()))?;
    // Appending to a file that already exists does not change the directory, so syncing it buys
    // nothing and costs as much as the write on a slow filesystem. A file created by this call does
    // need it — the name itself has to survive a crash.
    if created {
        if let Some(parent) = path.parent() {
            util::sync_dir(parent);
        }
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    /// Bytes this pass read from the cache files.
    pub bytes_read: u64,
    /// Times this pass wrote the whole base file.
    pub base_rewrites: usize,
    /// Delta records this pass appended.
    pub delta_records: usize,
    /// Delta records that were on disk when the pass started.
    pub delta_records_before: usize,
    /// Times a pass folded the delta into a new base because the delta had grown too large.
    pub compactions: usize,
    /// Set when a legacy `state.json` was migrated to the new format by this pass.
    pub migrated: Option<String>,
    /// Set when the cache was rebuilt from the journal (it was missing or damaged).
    pub rebuilt_from_journal: bool,
}

pub struct CacheStore {
    dir: PathBuf,
    /// A full pass rebuilds the base from the walk; a partial pass stages delta records.
    full: bool,
    base: Option<SortedFile>,
    base_entries: usize,
    delta: Delta,
    staged: Vec<Rec>,
    staged_ix: HashMap<String, usize>,
    /// Full mode: the state as it was, and the state this pass is building.
    old: BTreeMap<String, CacheEntry>,
    new: BTreeMap<String, CacheEntry>,
    /// The cache was rebuilt from the journal, so the pass owns the whole state (like a full pass).
    seeded: bool,
    /// Bytes read through base handles that have since been replaced, and how much of them is
    /// already inside `stats.bytes_read`.
    base_closed: u64,
    base_counted: u64,
    delta_cap: usize,
    /// How many paths were tracked at the start of the pass. Read from `cache/tracked.json` and
    /// checked against the delta's size, so counting costs a stat and not a scan.
    tracked: usize,
    pub stats: CacheStats,
    pub notes: Vec<String>,
}

impl CacheStore {
    /// Open the cache for one pass. `full` says a full pass is running (it will rewrite the base).
    pub fn open(project: &Project, full: bool, delta_cap: usize) -> Result<CacheStore, String> {
        let dir = project.dir.join("cache");
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut notes = Vec::new();
        let mut stats = CacheStats::default();
        let base_path = dir.join("base.jsonl");
        let legacy = dir.join("state.json");
        if !base_path.exists() && legacy.exists() {
            match migrate_legacy(&dir) {
                Ok(desc) => {
                    notes.push(desc.clone());
                    stats.migrated = Some(desc);
                }
                Err(e) => notes.push(format!("legacy cache could not be migrated ({e}); it will be rebuilt from the journal")),
            }
        }
        let base = SortedFile::open(&base_path)?;
        let mut base_entries = 0usize;
        if base.is_some() {
            match read_base_meta(&dir) {
                Some(n) => base_entries = n,
                // No usable meta: count instead of guessing (only after an interrupted write).
                None => base_entries = base.as_ref().map(|b| b.read_all().map(|v| v.len()).unwrap_or(0)).unwrap_or(0),
            }
        }
        let delta = Delta::open(&dir.join("delta.jsonl"))?;
        stats.bytes_read += delta.bytes;
        let mut notes2: Vec<String> = Vec::new();
        if let Some(d) = &delta.damaged {
            notes2.push(format!("the cache delta is damaged ({d}); the cache will be rebuilt from the journal"));
        }
        stats.delta_records_before = delta.len();
        // The count of tracked paths: the base's own count plus what the delta's records say they
        // did. Records that carry no `was` (none are written any more) are resolved against the base
        // once per path — the slow, always-correct path.
        let tracked = if delta.unwindable.is_empty() {
            (base_entries as i64 + delta.net).max(0) as usize
        } else {
            notes2.push(format!(
                "{} delta record(s) carry no transition; the tracked count was recomputed from the base",
                delta.unwindable.len()
            ));
            let mut n = base_entries as i64;
            let mut seen: HashMap<String, bool> = HashMap::new();
            for r in delta.all()? {
                let was = match r.was {
                    Some(w) => w,
                    None => *seen.get(&r.path).unwrap_or(&base_is_tracked(&base, &r.path)),
                };
                let now = matches!(r.op, Op::Set(_));
                match (was, now) {
                    (false, true) => n += 1,
                    (true, false) => n -= 1,
                    _ => {}
                }
                seen.insert(r.path.clone(), now);
            }
            n.max(0) as usize
        };
        let mut store = CacheStore {
            dir,
            full,
            base,
            base_entries,
            delta,
            staged: Vec::new(),
            staged_ix: HashMap::new(),
            old: BTreeMap::new(),
            new: BTreeMap::new(),
            seeded: false,
            delta_cap,
            tracked,
            base_closed: 0,
            base_counted: 0,
            stats,
            notes,
        };
        store.notes.extend(notes2);
        if full {
            let all = match &store.base {
                Some(b) => b.read_all()?,
                None => Vec::new(),
            };
            for (p, v) in all {
                store.old.insert(p, entry_of(&v)?);
            }
            for r in store.delta.all()? {
                match r.op {
                    Op::Set(e) => {
                        store.old.insert(r.path.clone(), e);
                    }
                    Op::Del => {
                        store.old.remove(&r.path);
                    }
                }
            }
        }
        Ok(store)
    }

    /// Was there any state to start from? A pass that finds none must rebuild from the journal.
    pub fn had_state(&self) -> bool {
        (self.base.is_some() || !self.delta.is_empty()) && self.delta.damaged.is_none()
    }

    /// The cache is gone or damaged: take the state from the journal instead (the old behaviour, and
    /// the only case in which a partial pass reads the journal — it is logged when it happens).
    pub fn seed_from_journal(&mut self, st: BTreeMap<String, CacheEntry>) {
        let n = st.len();
        self.old = st;
        self.seeded = true;
        self.stats.rebuilt_from_journal = true;
        self.notes.push(format!("the cache was missing or damaged: rebuilt from the journal ({n} entries)"));
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    /// How many paths were tracked when the pass started: `cache/tracked.json`, checked against the
    /// delta's exact size (see `open`). Every record carries the transition it made, so appending
    /// records updates this number without any further lookups.
    pub fn tracked_before(&self) -> usize {
        if self.full || self.seeded {
            return self.old.len();
        }
        self.tracked
    }

    /// The entry for one path: what this pass has already decided, then the delta (last record
    /// wins), then the base.
    pub fn get(&self, path: &str) -> Option<CacheEntry> {
        if self.full || self.seeded {
            return self.old.get(path).cloned();
        }
        if let Some(i) = self.staged_ix.get(path) {
            return match &self.staged[*i].op {
                Op::Set(e) => Some(e.clone()),
                Op::Del => None,
            };
        }
        // Only the records this path has: the delta is not parsed to answer a question about one
        // path, and it is not even read when nothing needs it.
        if self.delta.mentions(path) {
            if let Ok(Some(r)) = self.delta.last(path) {
                return match r.op {
                    Op::Set(e) => Some(e),
                    Op::Del => None,
                };
            }
        }
        match &self.base {
            Some(b) => b.find(path).ok().flatten().and_then(|v| entry_of(&v).ok()),
            None => None,
        }
    }

    /// Every tracked entry this pass is allowed to reason about.
    ///
    /// A full pass walks everything, so its candidate set is the whole state. A partial pass reads
    /// only what its scope covers: the base subtree under each named directory, the record of each
    /// named path, and the delta records for those paths — exactly "the cache of the affected paths"
    /// and nothing else.
    pub fn candidates(&self, scope: Option<&crate::scan::Scope>) -> Result<BTreeMap<String, CacheEntry>, String> {
        let mut out: BTreeMap<String, CacheEntry> = BTreeMap::new();
        if self.full || self.seeded {
            for (p, e) in &self.old {
                match scope {
                    Some(sc) if !self.full => {
                        if sc.covers(p) {
                            out.insert(p.clone(), e.clone());
                        }
                    }
                    _ => {
                        out.insert(p.clone(), e.clone());
                    }
                }
            }
            return Ok(out);
        }
        let sc = match scope {
            Some(s) => s,
            None => return Err("a partial cache read needs a scope".into()),
        };
        if let Some(b) = &self.base {
            for d in &sc.dirs {
                // The named path itself may be a tracked file that has since vanished (a notification
                // about a deleted file is classified as a directory — it has no type any more).
                if let Ok(Some(v)) = b.find(d) {
                    out.insert(d.clone(), entry_of(&v)?);
                }
                let prefix = if d.is_empty() { String::new() } else { format!("{d}/") };
                for (p, v) in b.scan_prefix(&prefix)? {
                    out.insert(p, entry_of(&v)?);
                }
            }
            for f in &sc.files {
                if out.contains_key(f) {
                    continue;
                }
                if let Ok(Some(v)) = b.find(f) {
                    out.insert(f.clone(), entry_of(&v)?);
                }
            }
        }
        for r in self.delta.covered(sc)? {
            match r.op {
                Op::Set(e) => {
                    out.insert(r.path.clone(), e);
                }
                Op::Del => {
                    out.remove(&r.path);
                }
            }
        }
        Ok(out)
    }

    /// Remember the entry for a path. A full pass must write every kept path (the base is rebuilt
    /// from these); a partial pass stages only what it touched.
    pub fn set(&mut self, path: &str, entry: CacheEntry) {
        if self.full || self.seeded {
            self.new.insert(path.to_string(), entry);
        } else {
            self.stage(path, Op::Set(entry));
        }
    }

    /// The path is no longer tracked (it was deleted, or it left the filters).
    pub fn del(&mut self, path: &str) {
        if self.full || self.seeded {
            self.new.remove(path);
            self.old.remove(path);
        } else {
            self.stage(path, Op::Del);
        }
    }

    /// Stage one change. `was` is read here, once, from the same overlay the record will be applied
    /// to — so the record carries the transition it made.
    fn stage(&mut self, path: &str, op: Op) {
        // `was` is the state this record changes — read from the same overlay the record will be
        // applied to. The count of tracked paths is derived from these flags at open time, so the
        // pass's own `tracked_before` must not move while it stages.
        let was = self.get(path).is_some();
        let r = Rec { path: path.to_string(), op, at: util::now_ms(), was: Some(was) };
        match self.staged_ix.get(&r.path) {
            Some(i) => {
                let i = *i;
                self.staged[i] = r;
            }
            None => {
                self.staged_ix.insert(r.path.clone(), self.staged.len());
                self.staged.push(r);
            }
        }
    }

    /// Write what this pass decided, and nothing else.
    pub fn commit(&mut self, project: &Project) -> Result<(), String> {
        let _ = project;
        if self.full {
            let map = std::mem::take(&mut self.new);
            return self.write_base(&map, "full");
        }
        if self.seeded {
            // The state came from the journal, so the base has to be written in full — this is the
            // rebuild case, and it is counted and reported.
            let mut map = std::mem::take(&mut self.old);
            let staged = std::mem::take(&mut self.staged);
            self.staged_ix.clear();
            for r in staged {
                match r.op {
                    Op::Set(e) => {
                        map.insert(r.path, e);
                    }
                    Op::Del => {
                        map.remove(&r.path);
                    }
                }
            }
            for (p, e) in std::mem::take(&mut self.new) {
                map.insert(p, e);
            }
            return self.write_base(&map, "rebuild");
        }
        if self.staged.is_empty() {
            return Ok(());
        }
        let staged_bytes: usize = self.staged.iter().map(|r| encode_rec(r).map(|l| l.len() + 1).unwrap_or(0)).sum();
        if self.delta_cap > 0 && (self.delta.bytes as usize + staged_bytes) > self.delta_cap {
            // The delta has grown past its cap: fold it into a new base now (rare, and it is the only
            // case other than a rebuild where a partial pass writes the base file).
            let mut map: BTreeMap<String, CacheEntry> = BTreeMap::new();
            if let Some(b) = &self.base {
                for (p, v) in b.read_all()? {
                    map.insert(p, entry_of(&v)?);
                }
            }
            for r in self.delta.all()? {
                match r.op {
                    Op::Set(e) => {
                        map.insert(r.path, e);
                    }
                    Op::Del => {
                        map.remove(&r.path);
                    }
                }
            }
            for r in self.staged.drain(..) {
                match r.op {
                    Op::Set(e) => {
                        map.insert(r.path, e);
                    }
                    Op::Del => {
                        map.remove(&r.path);
                    }
                }
            }
            self.staged_ix.clear();
            self.stats.compactions += 1;
            return self.write_base(&map, "compaction");
        }
        let staged = std::mem::take(&mut self.staged);
        self.staged_ix.clear();
        let mut payload = String::new();
        for r in &staged {
            payload.push_str(&encode_rec(r)?);
            payload.push('\n');
        }
        let n = staged.len();
        append_lines(&self.dir.join("delta.jsonl"), &payload)?;
        self.delta = Delta::open(&self.dir.join("delta.jsonl"))?;
        self.stats.delta_records += n;
        Ok(())
    }

    fn write_base(&mut self, map: &BTreeMap<String, CacheEntry>, cause: &str) -> Result<(), String> {
        // The handle is about to be replaced: its bytes are banked so the total stays complete.
        if let Some(b) = &self.base {
            self.base_closed += b.bytes_read();
            self.base_counted += b.bytes_read();
        }
        write_base_files(&self.dir, map, cause)?;
        self.base_entries = map.len();
        self.delta = Delta::open(&self.dir.join("delta.jsonl"))?;
        self.tracked = map.len();
        self.stats.base_rewrites += 1;
        self.base = SortedFile::open(&self.dir.join("base.jsonl"))?;
        Ok(())
    }

    /// Every tracked entry, for a pass that walked everything (or rebuilt from the journal). `None`
    /// means the store is in partial mode and only `candidates(scope)` may be read.
    pub fn all_entries(&self) -> Option<&BTreeMap<String, CacheEntry>> {
        if self.full || self.seeded {
            Some(&self.old)
        } else {
            None
        }
    }

    /// How many paths are tracked after this pass decided. Only the initial snapshot needs it, and
    /// that is always a full pass.
    pub fn tracked_after(&self) -> usize {
        if self.full {
            self.new.len()
        } else {
            self.tracked_before()
        }
    }

    /// Bytes read from the cache files by this pass: the base handle in use plus everything read
    /// through a handle that has since been replaced. Idempotent — asking twice gives the same
    /// number (the first version of this added the base's bytes to a total that already held them,
    /// and reported exactly twice what the pass had read).
    pub fn bytes_read(&self) -> u64 {
        self.stats.bytes_read + self.base_closed + self.base.as_ref().map(|b| b.bytes_read()).unwrap_or(0) - self.base_counted
    }
}

/// The whole tracked state, read in full. Used by callers that genuinely want every entry
/// (`pl apply-filters`, the tests) — never by the observation cycle.
pub fn materialize(project: &Project) -> Result<BTreeMap<String, CacheEntry>, String> {
    let mut store = CacheStore::open(project, true, 0)?;
    Ok(std::mem::take(&mut store.old))
}

/// Write a whole state as the base, and empty the delta. `pl rebuild-cache` and the migration use
/// this; the observation cycle writes through `CacheStore`.
pub fn write_snapshot(project: &Project, map: &BTreeMap<String, CacheEntry>, cause: &str) -> Result<(), String> {
    let dir = project.dir.join("cache");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write_base_files(&dir, map, cause)
}

fn write_base_files(dir: &Path, map: &BTreeMap<String, CacheEntry>, cause: &str) -> Result<(), String> {
    let mut out = String::new();
    for (p, e) in map {
        out.push_str(&record_line(p, e)?);
        out.push('\n');
    }
    util::write_atomic(&dir.join("base.jsonl"), out.as_bytes()).map_err(|e| e.to_string())?;
    // The one crash window of this format: the base is in place and the delta still holds records
    // that are already folded into it. Folding is idempotent, so the next reader is correct either
    // way — and this is where a test kills a real process to prove it.
    crate::lifecycle::crash_if("cache_base_written");
    let meta = serde_json::json!({
        "version": CACHE_FORMAT_VERSION,
        "entries": map.len(),
        "bytes": out.len(),
        "written": iso_ms(util::now_ms()),
        "cause": cause,
    });
    util::write_atomic(&dir.join("base.meta.json"), meta.to_string().as_bytes()).map_err(|e| e.to_string())?;
    // Order matters: the base is renamed into place first, then the delta is emptied. A crash in
    // between leaves records that are already folded in — and folding is idempotent, so the next
    // reader gets the same state. The other order would lose them.
    util::write_atomic(&dir.join("delta.jsonl"), b"").map_err(|e| e.to_string())?;
    // The other side of the same window: a crash here has the new base and an emptied delta — the
    // fold is complete and the next reader must find exactly the same state.
    crate::lifecycle::crash_if("cache_delta_reset");
    Ok(())
}

/// Is this path in the base file? (Only used to resolve a record that carries no `was`.)
fn base_is_tracked(base: &Option<SortedFile>, path: &str) -> bool {
    match base {
        Some(b) => b.find(path).map(|v| v.is_some()).unwrap_or(false),
        None => false,
    }
}

/// `cache/base.meta.json`: the entry count, validated against the base's size. It exists so that
/// "how many paths are tracked" is not a scan of the file.
fn read_base_meta(dir: &Path) -> Option<usize> {
    let text = fs::read_to_string(dir.join("base.meta.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("version").and_then(|x| x.as_u64()) != Some(CACHE_FORMAT_VERSION) {
        return None;
    }
    let entries = v.get("entries").and_then(|x| x.as_u64())? as usize;
    let bytes = v.get("bytes").and_then(|x| x.as_u64())?;
    let on_disk = fs::metadata(dir.join("base.jsonl")).ok()?.len();
    if bytes != on_disk {
        return None;
    }
    Some(entries)
}

/// The format-1 cache is one JSON object. It is read once, written as a sorted base, and the old
/// file is kept under a new name (nothing is deleted), so the migration can be inspected afterwards.
fn migrate_legacy(dir: &Path) -> Result<String, String> {
    let legacy = dir.join("state.json");
    let text = fs::read_to_string(&legacy).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", legacy.display()))?;
    let files = v.get("files").and_then(|f| f.as_object()).ok_or("the legacy cache has no `files`")?;
    let mut map: BTreeMap<String, CacheEntry> = BTreeMap::new();
    for (k, val) in files {
        map.insert(k.clone(), entry_of(val)?);
    }
    let mut out = String::new();
    for (p, e) in &map {
        out.push_str(&record_line(p, e)?);
        out.push('\n');
    }
    util::write_atomic(&dir.join("base.jsonl"), out.as_bytes()).map_err(|e| e.to_string())?;
    let meta = serde_json::json!({
        "version": CACHE_FORMAT_VERSION,
        "entries": map.len(),
        "bytes": out.len(),
        "written": iso_ms(util::now_ms()),
        "cause": "migration-from-v1",
    });
    util::write_atomic(&dir.join("base.meta.json"), meta.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let kept = dir.join(format!("state.json.v1-{}", util::now_ms()));
    fs::rename(&legacy, &kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    util::sync_dir(dir);
    Ok(format!(
        "cache format 1 migrated to 2: {} entries written as a sorted base, the old file kept as {}",
        map.len(),
        kept.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    ))
}

/// How many entries the base holds and how many delta records follow it (for `pl doctor`).
pub fn counts(project: &Project) -> Result<(usize, usize), String> {
    let dir = project.dir.join("cache");
    let base = SortedFile::open(&dir.join("base.jsonl"))?;
    let entries = match base {
        Some(b) => match read_base_meta(&dir) {
            Some(n) => n,
            None => b.read_all()?.len(),
        },
        None => 0,
    };
    let delta = Delta::open(&dir.join("delta.jsonl"))?.len();
    Ok((entries, delta))
}

// ---------------------------------------------------------------------------------------------
// The skip map: which (path, reason) pairs are already in the journal.
// ---------------------------------------------------------------------------------------------

/// `skip` events are written once per (path, reason). The last reason per path used to be found by
/// scanning the whole journal backwards on every pass; it is now a sorted file read by point lookup.
pub struct SkipMap {
    path: PathBuf,
    file: Option<SortedFile>,
    dirty: BTreeMap<String, String>,
    /// Pairs taken from the journal by a full pass that already had the events in memory.
    seeded: BTreeMap<String, String>,
    pub bytes_read: u64,
}

impl SkipMap {
    pub fn open(dir: &Path) -> Result<SkipMap, String> {
        let path = dir.join("skips.jsonl");
        let file = SortedFile::open(&path)?;
        Ok(SkipMap { path, file, dirty: BTreeMap::new(), seeded: BTreeMap::new(), bytes_read: 0 })
    }

    /// Remember a pair the journal already holds (a full pass reads the journal anyway, so the file
    /// can be seeded from memory instead of re-reading it).
    pub fn seed(&mut self, path: &str, reason: &str) {
        self.seeded.entry(path.to_string()).or_insert_with(|| reason.to_string());
    }

    /// Is the journal already holding this skip, with this reason?
    pub fn recorded(&self, path: &str, reason: &str) -> Result<bool, String> {
        if let Some(r) = self.dirty.get(path) {
            return Ok(r == reason);
        }
        if let Some(r) = self.seeded.get(path) {
            return Ok(r == reason);
        }
        match &self.file {
            Some(f) => Ok(match f.find(path)? {
                Some(v) => v.get("reason").and_then(|x| x.as_str()) == Some(reason),
                None => false,
            }),
            None => Ok(false),
        }
    }

    pub fn set(&mut self, path: &str, reason: &str) {
        self.dirty.insert(path.to_string(), reason.to_string());
    }

    /// Rewrite the file when this pass changed something. It is small (one line per skipped path),
    /// and it only changes when a path starts being skipped, or for a different reason.
    pub fn commit(&mut self) -> Result<bool, String> {
        if let Some(f) = &self.file {
            self.bytes_read = self.bytes_read.max(f.bytes_read());
        }
        if self.dirty.is_empty() && self.seeded.is_empty() {
            return Ok(false);
        }
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        if let Some(f) = &self.file {
            for (p, v) in f.read_all()? {
                if let Some(r) = v.get("reason").and_then(|x| x.as_str()) {
                    map.insert(p, r.to_string());
                }
            }
            self.bytes_read = self.bytes_read.max(f.bytes_read());
        }
        for (p, r) in &self.seeded {
            map.entry(p.clone()).or_insert_with(|| r.clone());
        }
        for (p, r) in &self.dirty {
            map.insert(p.clone(), r.clone());
        }
        let mut out = String::new();
        for (p, r) in &map {
            out.push_str(&serde_json::to_string(&serde_json::json!({ "path": p, "reason": r })).map_err(|e| e.to_string())?);
            out.push('\n');
        }
        util::write_atomic(&self.path, out.as_bytes()).map_err(|e| e.to_string())?;
        self.file = SortedFile::open(&self.path)?;
        self.dirty.clear();
        self.seeded.clear();
        Ok(true)
    }
}
