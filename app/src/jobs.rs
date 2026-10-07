//! Background work.
//!
//! Adding a folder copies every matching file; restoring and exporting read the whole archive. None
//! of that belongs on the request that the window is waiting for, so each one becomes a job with an
//! id, and the UI polls it while the core prints what it is doing.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct JobHandle {
    lines: Arc<Mutex<Vec<String>>>,
}

impl JobHandle {
    pub fn line(&self, s: &str) {
        if let Ok(mut l) = self.lines.lock() {
            if l.len() >= 400 {
                return;
            }
            l.push(s.to_string());
        }
    }
}

#[derive(Clone)]
pub struct JobView {
    pub id: u64,
    pub kind: String,
    pub label: String,
    pub state: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub exit: Option<i32>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub lines: Vec<String>,
}

impl JobView {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "label": self.label,
            "state": self.state,
            "startedMs": self.started_ms,
            "endedMs": self.ended_ms,
            "exit": self.exit,
            "result": self.result,
            "error": self.error,
            "lines": self.lines,
        })
    }
}

struct JobRecord {
    view: JobView,
    lines: Arc<Mutex<Vec<String>>>,
}

pub struct Registry {
    jobs: Mutex<HashMap<u64, JobRecord>>,
    next: AtomicU64,
}

impl Registry {
    pub fn new() -> Arc<Registry> {
        Arc::new(Registry { jobs: Mutex::new(HashMap::new()), next: AtomicU64::new(1) })
    }

    /// Start a job. The closure runs on its own thread and gets a handle to append output lines.
    pub fn start<F>(self: &Arc<Self>, kind: &str, label: &str, work: F) -> u64
    where
        F: FnOnce(&JobHandle) -> Result<Value, String> + Send + 'static,
    {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let lines = Arc::new(Mutex::new(Vec::new()));
        let view = JobView {
            id,
            kind: kind.to_string(),
            label: label.to_string(),
            state: "running".into(),
            started_ms: crate::pl::now_ms(),
            ended_ms: None,
            exit: None,
            result: None,
            error: None,
            lines: Vec::new(),
        };
        if let Ok(mut m) = self.jobs.lock() {
            m.insert(id, JobRecord { view, lines: lines.clone() });
        }
        let handle = JobHandle { lines };
        let h2 = handle.clone();
        let jobs = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome = work(&h2);
            if let Ok(mut m) = jobs.jobs.lock() {
                if let Some(rec) = m.get_mut(&id) {
                    rec.view.ended_ms = Some(crate::pl::now_ms());
                    match outcome {
                        Ok(v) => {
                            rec.view.state = "done".into();
                            rec.view.exit = Some(0);
                            rec.view.result = Some(v);
                        }
                        Err(e) => {
                            rec.view.state = "failed".into();
                            rec.view.exit = Some(1);
                            rec.view.error = Some(e);
                        }
                    }
                }
            }
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<JobView> {
        let mut m = self.jobs.lock().ok()?;
        let rec = m.get_mut(&id)?;
        rec.view.lines = rec.lines.lock().map(|l| l.clone()).unwrap_or_default();
        Some(rec.view.clone())
    }

    /// Is a job of this kind still running? Used to keep the UI from starting two adds at once.
    pub fn running_of_kind(&self, kind: &str) -> Option<u64> {
        let m = self.jobs.lock().ok()?;
        m.values().filter(|r| r.view.state == "running" && r.view.kind == kind).map(|r| r.view.id).max()
    }

    pub fn list(&self) -> Vec<JobView> {
        let mut v: Vec<JobView> = match self.jobs.lock() {
            Ok(mut m) => m
                .values_mut()
                .map(|r| {
                    let mut view = r.view.clone();
                    view.lines = r.lines.lock().map(|l| l.clone()).unwrap_or_default();
                    view
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by_key(|j| std::cmp::Reverse(j.id));
        v.truncate(20);
        v
    }
}

/// Run a core command and stream its output into the job's log, returning (exit code, stdout).
pub fn run_streaming(core: &crate::pl::Core, args: &[String], h: &JobHandle) -> Result<(i32, String), String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    h.line(&format!("$ projectlife {}", args.join(" ")));
    let mut child = core
        .command(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run the core: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let h2 = h.clone();
    let t = std::thread::spawn(move || {
        let r = BufReader::new(stderr);
        for line in r.lines().map_while(Result::ok) {
            h2.line(&format!("! {line}"));
        }
    });
    let mut out = String::new();
    let r = BufReader::new(stdout);
    for line in r.lines().map_while(Result::ok) {
        h.line(&line);
        out.push_str(&line);
        out.push('\n');
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = t.join();
    Ok((status.code().unwrap_or(-1), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_reports_its_result_and_its_lines() {
        let reg = Registry::new();
        let id = reg.start("test", "a job that succeeds", |h| {
            h.line("one");
            h.line("two");
            Ok(json!({"answer": 42}))
        });
        let mut last = None;
        for _ in 0..200 {
            let j = reg.get(id).expect("the job exists");
            if j.state != "running" {
                last = Some(j);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let j = last.expect("the job finished");
        assert_eq!(j.state, "done");
        assert_eq!(j.exit, Some(0));
        assert_eq!(j.result.unwrap()["answer"], 42);
        assert_eq!(j.lines, vec!["one".to_string(), "two".to_string()]);
        assert!(reg.running_of_kind("test").is_none());
    }

    #[test]
    fn a_job_that_fails_says_why() {
        let reg = Registry::new();
        let id = reg.start("test", "a job that fails", |h| {
            h.line("starting");
            Err("the core refused".to_string())
        });
        for _ in 0..200 {
            let j = reg.get(id).unwrap();
            if j.state != "running" {
                assert_eq!(j.state, "failed");
                assert_eq!(j.error.as_deref(), Some("the core refused"));
                assert_eq!(j.exit, Some(1));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the job never finished");
    }
}
