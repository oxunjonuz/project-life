//! Operation log: what the PROGRAM did (restore, prune, retention, export, drill, panic).
//!
//! The project journal records what the *filesystem* did; this file records what *we* did to it.
//! Keeping the two apart is not bookkeeping for its own sake: it means an operation that writes
//! into the project folder never has to append to the history, and `recent`, `undo` and `suggest`
//! can answer "what happened last?" without taking a writer lock or touching the archive of any
//! single project. The file is append-only, one JSON object per line, and a half-written trailing
//! line is ignored rather than fatal.

use crate::archive::Archive;
use crate::util;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub fn path_of(archive: &Archive) -> PathBuf {
    archive.root.join("logs").join("operations.jsonl")
}

/// Append one operation. Failure to log is never allowed to fail the operation itself, but it is
/// also never silent: it is written into the ordinary archive log.
pub fn record(archive: &Archive, op: &str, project: &str, detail: Value) {
    let dir = archive.root.join("logs");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = util::now_ms();
    let mut obj = serde_json::Map::new();
    obj.insert("ts".into(), Value::from(now));
    obj.insert("time".into(), Value::from(util::fmt_local(now)));
    obj.insert("op".into(), Value::from(op));
    obj.insert("project".into(), Value::from(project));
    obj.insert("pid".into(), Value::from(std::process::id()));
    if let Value::Object(m) = detail {
        for (k, v) in m {
            obj.insert(k, v);
        }
    }
    let line = serde_json::to_string(&Value::Object(obj)).unwrap_or_default();
    match fs::OpenOptions::new().create(true).append(true).open(dir.join("operations.jsonl")) {
        Ok(mut f) => {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
        Err(e) => archive.log(&format!("could not write the operation log: {e}")),
    }
}

/// Every record, oldest first. Unreadable or half-written lines are skipped, not fatal.
pub fn read_all(archive: &Archive) -> Vec<Value> {
    let text = match fs::read_to_string(path_of(archive)) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                serde_json::from_str::<Value>(t).ok()
            }
        })
        .collect()
}

/// The newest record of one kind (optionally for one project).
pub fn last_of(archive: &Archive, op: &str, project: Option<&str>) -> Option<Value> {
    let mut all = read_all(archive);
    all.reverse();
    all.into_iter().find(|v| {
        v.get("op").and_then(|x| x.as_str()) == Some(op)
            && match project {
                Some(p) => v.get("project").and_then(|x| x.as_str()) == Some(p),
                None => true,
            }
    })
}

pub fn ts_of(v: &Value) -> i64 {
    v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0)
}
