//! Archive: root layout, configuration, project passport, lock, heartbeat, free space, log.

use crate::util;
use serde_json::{Map, Value};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const LOCATION_REL: &str = "projectlife/location.json";

/// Global configuration — plain JSON with defaults.
#[derive(Clone, Debug)]
pub struct Config {
    pub map: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        let mut m = Map::new();
        m.insert("intervalSeconds".into(), Value::from(5));
        m.insert("minIntervalSeconds".into(), Value::from(1));
        m.insert("maxIntervalSeconds".into(), Value::from(60));
        m.insert("autoInterval".into(), Value::from(true));
        m.insert("debounceMs".into(), Value::from(1500));
        // Round 290: the filesystem-notification trigger. On by default where a backend exists;
        // `false` leaves the periodic pass as the only trigger (the measured "without" case).
        m.insert("watchTriggers".into(), Value::from(true));
        // How many cycles between full watch-set re-syncs (the self-healing path).
        m.insert("triggerResyncCycles".into(), Value::from(12));
        m.insert("deepVerifyIntervalMinutes".into(), Value::from(60));
        m.insert("profile".into(), Value::from("source"));
        m.insert("warnFreePercent".into(), Value::from(10));
        m.insert("warnFreeBytes".into(), Value::from(5u64 * 1024 * 1024 * 1024));
        m.insert("stopFreePercent".into(), Value::from(1));
        m.insert("stopFreeBytes".into(), Value::from(500u64 * 1024 * 1024));
        m.insert("massChangeFiles".into(), Value::from(50));
        m.insert("massChangePercent".into(), Value::from(30));
        m.insert("minMassFiles".into(), Value::from(3));
        m.insert("lowPriority".into(), Value::from(true));
        m.insert("maxConcurrentReads".into(), Value::from(4));
        m.insert("language".into(), Value::from("en"));
        m.insert("notifications".into(), Value::from(true));
        Config { map: m }
    }
}

impl Config {
    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();
        let p = root.join("config.json");
        if let Ok(text) = fs::read_to_string(&p) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) {
                for (k, v) in m {
                    cfg.map.insert(k, v);
                }
            }
        }
        cfg
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&Value::Object(self.map.clone())).unwrap_or_else(|_| "{}".into());
        util::write_atomic(&root.join("config.json"), text.as_bytes())
    }

    /// The bytes of `config.json` as a sha256, or `"absent"` when there is no file.
    pub fn fingerprint(root: &Path) -> String {
        match fs::read(root.join("config.json")) {
            Ok(bytes) => {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(&bytes);
                format!("{:x}", h.finalize())
            }
            Err(_) => "absent".to_string(),
        }
    }

    pub fn u64_of(&self, key: &str, def: u64) -> u64 {
        self.map.get(key).and_then(|v| v.as_u64()).unwrap_or(def)
    }
    pub fn i64_of(&self, key: &str, def: i64) -> i64 {
        self.map
            .get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
            .unwrap_or(def)
    }
    pub fn bool_of(&self, key: &str, def: bool) -> bool {
        self.map.get(key).and_then(|v| v.as_bool()).unwrap_or(def)
    }
    pub fn str_of(&self, key: &str, def: &str) -> String {
        self.map.get(key).and_then(|v| v.as_str()).unwrap_or(def).to_string()
    }
    pub fn set(&mut self, key: &str, value: Value) {
        self.map.insert(key.to_string(), value);
    }
}

pub struct Archive {
    pub root: PathBuf,
    pub config: Config,
    /// Round 300 (FR-CFG-3): the fingerprint of `config.json` the running configuration was read
    /// from. `None` means the file was absent. One sha256 of a few hundred bytes per cycle is the
    /// cheapest honest way to notice an edit — mtime alone misses two writes inside one second and
    /// hits false positives after a copy.
    config_fp: String,
    /// How many times a running process has re-read its configuration without restarting.
    pub config_reloads: u64,
}

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn release(self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The daemon's own file: held from `daemon run` until it stops, so a second start exits with a
/// clear message instead of quietly racing the first one through the per-cycle `.lock` (FR-WCH-12).
pub struct DaemonLock {
    path: PathBuf,
    pid: i32,
}

impl DaemonLock {
    pub fn release(self) {
        // Only ever remove our own file: if it names another process, a newer daemon has taken over.
        if let Ok(raw) = fs::read_to_string(&self.path) {
            if parse_lock(&raw).pid == Some(self.pid) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// What the lock file says: who holds it, which process, since when.
#[derive(Clone, Debug)]
pub struct LockInfo {
    pub raw: String,
    pub who: String,
    pub pid: Option<i32>,
    pub since: Option<i64>,
}

/// The lock line format written by this program: `<who> <pid> <epoch-ms>`.
/// Older builds wrote `<who> <epoch-ms>` with no pid; both are understood.
pub fn parse_lock(raw: &str) -> LockInfo {
    let t = raw.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    let nums: Vec<i64> = words.iter().filter_map(|w| w.parse::<i64>().ok()).collect();
    let since = nums.last().copied().filter(|n| *n > 1_000_000_000_000);
    let pid = if nums.len() >= 2 {
        nums[nums.len() - 2].try_into().ok().filter(|p| *p > 0)
    } else {
        None
    };
    LockInfo { raw: t.to_string(), who: words.first().map(|s| s.to_string()).unwrap_or_default(), pid, since }
}

/// Is this process still running? On unix an existing process that is not ours answers EPERM,
/// which still means "alive". Where liveness cannot be established (not unix) the answer is
/// `true`, so a lock is never stolen from a live holder by a guess.
pub fn pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        crate::util::win::pid_alive(pid as u32)
    }
    // No way to ask: the answer that keeps a lock is "alive", which costs a `doctor --fix-lock` and
    // never costs a stolen lock from a process that is still writing.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

impl Archive {
    pub fn looks_like_archive(root: &Path) -> bool {
        root.join("config.json").exists() && root.join("projects").is_dir()
    }

    pub fn open(root: &Path) -> Result<Archive, String> {
        if !root.is_dir() {
            return Err(format!("archive not found: {}", root.display()));
        }
        if !Archive::looks_like_archive(root) {
            return Err(format!(
                "this is not a Project Life archive (no config.json / projects): {}\nCreate one: projectlife init-archive {}",
                root.display(),
                root.display()
            ));
        }
        Ok(Archive::with_config(root, Config::load(root)))
    }

    fn with_config(root: &Path, config: Config) -> Archive {
        Archive {
            root: root.to_path_buf(),
            config,
            config_fp: Config::fingerprint(root),
            config_reloads: 0,
        }
    }

    /// FR-CFG-3: re-read `<archive>/config.json` when its bytes differ from the ones the running
    /// configuration was read from. Returns the keys whose value changed, an empty list when the file
    /// changed but no value did, and `None` when nothing changed at all (the common case: one sha256).
    ///
    /// Keys read *per cycle* by the cycle itself (`notifications`, `partialPass`, the mass thresholds,
    /// the filters, `lowPriority`, …) are therefore live the moment the next cycle starts. The keys
    /// the loop holds in local variables (`intervalSeconds` and its family, `debounceMs`,
    /// `triggerResyncCycles`, `deepVerifyIntervalMinutes`, `watchTriggers`) are re-applied by the
    /// caller, which is the only place that can reschedule itself.
    pub fn reload_config(&mut self) -> Option<Vec<String>> {
        let fp = Config::fingerprint(&self.root);
        if fp == self.config_fp {
            return None;
        }
        let old = self.config.clone();
        let new = Config::load(&self.root);
        let mut keys: std::collections::BTreeSet<String> = old.map.keys().cloned().collect();
        keys.extend(new.map.keys().cloned());
        let mut changed: Vec<String> = Vec::new();
        for k in keys {
            if old.map.get(&k) != new.map.get(&k) {
                changed.push(k);
            }
        }
        self.config = new;
        self.config_fp = fp;
        self.config_reloads += 1;
        changed.sort();
        if changed.is_empty() {
            self.log("configuration file changed but no key's value differs: nothing to apply");
        } else {
            self.log(&format!(
                "configuration re-read without a restart (FR-CFG-3): {} changed",
                changed.join(", ")
            ));
        }
        Some(changed)
    }

    /// The fingerprint the running configuration was read from — for `daemon status`.
    pub fn config_fingerprint(&self) -> &str {
        &self.config_fp
    }

    /// A small cross-process record of the last configuration re-read, so `daemon status` and the
    /// window can show what a *running* process did — its counters are not reachable from another
    /// process, and a claim about live reloading that only exists in one process's memory is not
    /// checkable. Writes `<archive>/config_state.json`.
    pub fn write_config_state(&self, changed: &[String], why: &str) {
        let v = serde_json::json!({
            "at": util::now_ms(),
            "atIso": crate::archive::iso_ms(util::now_ms()),
            "fingerprint": self.config_fp,
            "reloads": self.config_reloads,
            "changedKeys": changed,
            "reason": why,
            "intervalSeconds": self.config.i64_of("intervalSeconds", 5),
            "watchTriggers": self.config.bool_of("watchTriggers", true),
        });
        let _ = util::write_atomic_lazy(
            &self.root.join("config_state.json"),
            serde_json::to_string_pretty(&v).unwrap_or_default().as_bytes(),
        );
    }

    pub fn create(root: &Path) -> Result<Archive, String> {
        fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        fs::create_dir_all(root.join("projects")).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("logs")).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("manifest")).map_err(|e| e.to_string())?;
        util::set_dir_owner_only(root);
        let arch = Archive::with_config(root, Config::load(root));
        arch.config.save(root).map_err(|e| e.to_string())?;
        let readme = root.join("README_RECOVERY.txt");
        if !readme.exists() {
            let _ = util::write_atomic(&readme, RECOVERY_TEXT.as_bytes());
        }
        Ok(arch)
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn log(&self, msg: &str) {
        let dir = self.root.join("logs");
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let line = format!("{} [{}] {}\n", util::fmt_local(util::now_ms()), std::process::id(), msg);
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("projectlife.log")) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub fn heartbeat_path(&self) -> PathBuf {
        self.root.join("heartbeat")
    }

    pub fn write_heartbeat(&self, ts: i64) {
        // A liveness signal, rewritten every cycle: no fsync (a stale heartbeat is what
        // `heartbeat-check` is *for*).
        let _ = util::write_atomic_lazy(&self.heartbeat_path(), format!("{ts}\n").as_bytes());
    }

    pub fn heartbeat_ms(&self) -> Option<i64> {
        fs::read_to_string(self.heartbeat_path())
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
    }

    pub fn free_bytes(&self) -> Option<u64> {
        free_space(&self.root)
    }

    pub fn total_bytes(&self) -> Option<u64> {
        total_space(&self.root)
    }

    /// Writer lock: one cycle at a time per archive (daemon and scan-once take the same lock).
    ///
    /// The lock line carries the holder's pid, so a lock left behind by `kill -9` is recognised as
    /// stale and taken over on the next cycle instead of blocking every writer until someone edits
    /// the file by hand. A live holder is never displaced; `doctor --fix-lock` is the one command
    /// that removes a lock whose owner is provably gone.
    pub fn lock(&self, who: &str) -> Result<Lock, String> {
        let path = self.root.join(".lock");
        self.take_lock_file(&path, who, "cycle")?;
        Ok(Lock { path })
    }

    /// The daemon-lifetime lock (`.daemon`). Same staleness rules as `.lock`: a file left behind by
    /// a process that is provably gone is taken over, a live holder is never displaced.
    pub fn daemon_lock(&self, who: &str) -> Result<DaemonLock, String> {
        let path = self.root.join(".daemon");
        self.take_lock_file(&path, who, "daemon")?;
        Ok(DaemonLock { path, pid: std::process::id() as i32 })
    }

    /// Who holds the daemon lock, if anyone.
    pub fn daemon_holder(&self) -> Option<LockInfo> {
        let raw = fs::read_to_string(self.root.join(".daemon")).ok()?;
        let info = parse_lock(&raw);
        if info.pid.map(pid_alive).unwrap_or(true) {
            Some(info)
        } else {
            None
        }
    }

    /// `create_new` first; on failure, decide from the file's own line whether the holder is gone.
    fn take_lock_file(&self, path: &Path, who: &str, kind: &str) -> Result<(), String> {
        let stale_after_ms = 120_000;
        for attempt in 0..2 {
            match fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut f) => {
                    let _ = f.write_all(format!("{who} {} {}\n", std::process::id(), util::now_ms()).as_bytes());
                    let _ = f.flush();
                    return Ok(());
                }
                Err(_) if attempt == 0 => {
                    let raw = fs::read_to_string(path).unwrap_or_default();
                    let info = parse_lock(&raw);
                    let age = info.since.map(|ts| util::now_ms() - ts).unwrap_or(0);
                    let dead = match info.pid {
                        Some(pid) => !pid_alive(pid),
                        // No pid recorded (an older build): only a file past the stale window is
                        // assumed abandoned.
                        None => age > stale_after_ms,
                    };
                    if !dead {
                        let holder = if info.pid.is_some() { info.raw.clone() } else { raw.trim().to_string() };
                        if kind == "daemon" {
                            return Err(format!(
                                "another daemon is already observing this archive (\"{holder}\"). \
                                 Stop it first: projectlife daemon stop   (or: projectlife doctor --fix-lock)"
                            ));
                        }
                        return Err(format!(
                            "archive is already in use (lock held by \"{holder}\"). A second writer cycle will not start.\n\
                             If you are sure that process is gone: projectlife doctor --fix-lock"
                        ));
                    }
                    // Take over atomically: renaming cannot race with another cycle doing the same.
                    let steal = self.root.join(format!(".{kind}.stale-{}", std::process::id()));
                    if fs::rename(path, &steal).is_err() {
                        return Err(format!("archive is already in use ({})", raw.trim()));
                    }
                    let _ = fs::remove_file(&steal);
                    let noun = if kind == "cycle" { "lock".to_string() } else { format!("{kind} lock") };
                    self.log(&format!(
                        "took over a stale {noun} left by \"{}\" (pid {:?}, age {} ms) — that process is gone",
                        info.who, info.pid, age
                    ));
                }
                Err(e) => return Err(format!("cannot take the {kind} lock {}: {e}", path.display())),
            }
        }
        Err(format!("archive is already in use ({})", path.display()))
    }

    /// Where a stop request is left for the daemon. One line, `"<pid> <ms>"`, and the pid is the
    /// whole point: a request is honoured only by the process it names, so a request that outlived
    /// the daemon it was written for can never stop the next one. This is the one stop mechanism
    /// every platform has — a signal needs a signal, and a detached Windows process has none.
    pub fn stop_request_path(&self) -> PathBuf {
        self.root.join("stop.request")
    }

    /// Write the request. The caller knows the pid from the `.daemon` lock (`daemon stop` does).
    pub fn request_stop(&self, pid: i32) -> Result<PathBuf, String> {
        let p = self.stop_request_path();
        let line = format!("{pid} {}\n", util::now_ms());
        util::write_atomic(&p, line.as_bytes()).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
        Ok(p)
    }

    /// Consume a request that names `pid`, and return its line. A request naming somebody else is
    /// left where it is: it belongs to another process, and removing it would be helping the wrong
    /// one. Removing it here is what makes the request honoured exactly once.
    pub fn take_stop_request(&self, pid: i32) -> Option<String> {
        let p = self.stop_request_path();
        let raw = fs::read_to_string(&p).ok()?;
        let line = raw.trim().to_string();
        let named: Option<i32> = line.split_whitespace().next().and_then(|w| w.parse().ok());
        if named == Some(pid) {
            let _ = fs::remove_file(&p);
            return Some(line);
        }
        None
    }

    /// Remove a request left behind by a daemon that is gone, remembering what it said. Called once
    /// at startup, so that "stop" meant for the previous process cannot stop this one.
    pub fn clear_stale_stop_request(&self) -> Option<String> {
        let p = self.stop_request_path();
        let raw = fs::read_to_string(&p).ok();
        let _ = fs::remove_file(&p);
        raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Who, if anyone, a stop request currently names.
    pub fn stop_request_holder(&self) -> Option<i32> {
        let raw = fs::read_to_string(self.stop_request_path()).ok()?;
        raw.split_whitespace().next().and_then(|w| w.parse().ok())
    }

    /// Remove a lock whose owner is provably gone. `force` removes it whatever it says.
    pub fn break_lock(&self, force: bool) -> Result<String, String> {
        let mut lines: Vec<String> = Vec::new();
        for (kind, path) in [("cycle", self.root.join(".lock")), ("daemon", self.root.join(".daemon"))] {
            let raw = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => {
                    lines.push(format!("no {kind} lock file: nothing to remove"));
                    continue;
                }
            };
            let info = parse_lock(&raw);
            let alive = info.pid.map(pid_alive).unwrap_or(false);
            if alive && !force {
                if kind == "daemon" {
                    return Err(format!(
                        "the daemon lock is held by a RUNNING process ({}). Stop it with `projectlife daemon stop`, \
                         or pass --force if you know it is not a writer.",
                        info.raw
                    ));
                }
                return Err(format!(
                    "the lock is held by a RUNNING process ({}). Not removing it — stop that process first, \
                     or pass --force if you know it is not a writer.",
                    info.raw
                ));
            }
            fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            lines.push(if alive {
                format!("removed the {kind} lock held by a running process ({}) because --force was given", info.raw)
            } else {
                format!("removed a stale {kind} lock ({})", info.raw)
            });
        }
        Ok(lines.join("\n"))
    }

    pub fn load_projects(&self) -> Result<Vec<Project>, String> {
        let mut out = Vec::new();
        let dir = self.projects_dir();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in util::read_dir_sorted(&dir).map_err(|e| e.to_string())? {
            if !entry.is_dir() {
                continue;
            }
            if let Ok(p) = Project::from_dir(&entry) {
                out.push(p);
            }
        }
        Ok(out)
    }

    pub fn find(&self, name_or_id: &str) -> Result<Project, String> {
        let projects = self.load_projects()?;
        let mut hits: Vec<Project> = projects
            .iter()
            .filter(|p| p.name == name_or_id || p.id == name_or_id)
            .cloned()
            .collect();
        if hits.is_empty() {
            hits = projects
                .iter()
                .filter(|p| {
                    p.name.starts_with(name_or_id)
                        || p.id.starts_with(name_or_id)
                        || p.project_root.ends_with(name_or_id)
                })
                .cloned()
                .collect();
        }
        match hits.len() {
            0 => Err(format!("project not found: {name_or_id}")),
            1 => Ok(hits.remove(0)),
            _ => Err(format!(
                "ambiguous name {name_or_id}: {}",
                hits.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub dir: PathBuf,
    pub id: String,
    pub name: String,
    pub project_root: String,
    pub meta: Value,
}

impl Project {
    pub fn from_dir(dir: &Path) -> Result<Project, String> {
        let text = fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
        let meta: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let id = meta.get("projectId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let project_root = meta.get("projectRoot").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if id.is_empty() {
            return Err("project.json has no projectId".into());
        }
        Ok(Project { dir: dir.to_path_buf(), id, name, project_root, meta })
    }

    pub fn save_meta(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.meta).map_err(|e| e.to_string())?;
        util::write_atomic(&self.dir.join("project.json"), text.as_bytes()).map_err(|e| e.to_string())
    }

    /// The same, without fsync. Round 294: everything an ordinary cycle writes into `project.json`
    /// (lastSeq, lastObservedAt, the observed-interval samples, the state) is derived from the
    /// journal, and a crash cannot lose anything that the journal does not already hold — while
    /// `pause`, `resume`, `prune` and `relink` write real state and keep the durable path.
    pub fn save_meta_lazy(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.meta).map_err(|e| e.to_string())?;
        util::write_atomic_lazy(&self.dir.join("project.json"), text.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn meta_str(&self, key: &str) -> Option<String> {
        self.meta.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    }

    pub fn meta_i64(&self, key: &str) -> Option<i64> {
        self.meta.get(key).and_then(|v| v.as_i64())
    }

    pub fn set_meta(&mut self, key: &str, value: Value) {
        if let Some(obj) = self.meta.as_object_mut() {
            obj.insert(key.to_string(), value);
        }
    }

    pub fn settings(&self) -> Map<String, Value> {
        self.meta
            .get("settings")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default()
    }

    pub fn settings_mut(&mut self) -> &mut Map<String, Value> {
        if self.meta.get("settings").and_then(|v| v.as_object()).is_none() {
            if let Some(obj) = self.meta.as_object_mut() {
                obj.insert("settings".into(), Value::Object(Map::new()));
            }
        }
        self.meta.get_mut("settings").and_then(|v| v.as_object_mut()).expect("settings object")
    }

    pub fn state(&self) -> String {
        self.meta_str("state").unwrap_or_else(|| "active".into())
    }

    pub fn profile(&self) -> String {
        self.meta_str("profile").unwrap_or_else(|| "source".into())
    }

    pub fn history_starts_at(&self) -> i64 {
        self.meta_str("historyStartsAt").and_then(|s| parse_iso_ms(&s)).unwrap_or(0)
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.dir.join("blobs")
    }
    pub fn events_dir(&self) -> PathBuf {
        self.dir.join("events")
    }
    /// The cache directory. Round 294: the state lives in `base.jsonl` + `delta.jsonl` here, and
    /// `state.json` is the format-1 file, read once and migrated on the first open.
    pub fn cache_dir(&self) -> PathBuf {
        self.dir.join("cache")
    }
    /// The format-1 cache file. Kept so old archives can be recognised and migrated; nothing writes
    /// it any more.
    pub fn cache_file(&self) -> PathBuf {
        self.dir.join("cache").join("state.json")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.join("tmp")
    }
    pub fn quarantine_dir(&self) -> PathBuf {
        self.dir.join("quarantine")
    }
    pub fn prune_journal(&self) -> PathBuf {
        self.dir.join("prune.journal")
    }
    pub fn project_path(&self) -> PathBuf {
        PathBuf::from(&self.project_root)
    }

    pub fn size_on_disk(&self) -> u64 {
        fn walk(p: &Path, acc: &mut u64) {
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if let Ok(md) = e.metadata() {
                        if md.is_dir() {
                            walk(&path, acc);
                        } else {
                            *acc += md.len();
                        }
                    }
                }
            }
        }
        let mut acc = 0;
        walk(&self.dir, &mut acc);
        acc
    }
}

/// Parse the UTC ISO form the program itself writes: `2026-10-05T08:12:44.120Z`.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let core = s.trim_end_matches('Z');
    let (date, time) = core.split_once('T')?;
    let mut d = date.split('-');
    let y: i32 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let (h, mi, sec, frac) = {
        let (main, frac) = match time.split_once('.') {
            Some((a, b)) => (a, b),
            None => (time, "0"),
        };
        let mut t = main.split(':');
        let h: u32 = t.next()?.parse().ok()?;
        let mi: u32 = t.next()?.parse().ok()?;
        let sec: u32 = t.next().unwrap_or("0").parse().ok()?;
        let frac: u32 = format!("{:0<3}", frac).chars().take(3).collect::<String>().parse().ok()?;
        (h, mi, sec, frac)
    };
    let days = days_from_civil(y, mo, day);
    Some(days * 86_400_000 + (h as i64) * 3_600_000 + (mi as i64) * 60_000 + (sec as i64) * 1000 + frac as i64)
}

pub fn iso_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let h = rem / 3_600_000;
    let mi = (rem % 3_600_000) / 60_000;
    let s = (rem % 60_000) / 1000;
    let milli = rem % 1000;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    unsafe {
        use std::ffi::CString;
        let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        crate::util::win::disk_space(path).map(|(avail, _, _)| avail)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

pub fn total_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    unsafe {
        use std::ffi::CString;
        let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some(st.f_blocks as u64 * st.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        crate::util::win::disk_space(path).map(|(_, total, _)| total)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// The archive location file lives in the OS config area, never inside a project.
pub fn location_file() -> PathBuf {
    if let Ok(home) = std::env::var("PROJECTLIFE_HOME") {
        return PathBuf::from(home).join("projectlife").join("location.json");
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join(LOCATION_REL);
        }
    }
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support").join(LOCATION_REL);
        }
    }
    if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join(LOCATION_REL);
        }
    }
    match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".config").join(LOCATION_REL),
        Err(_) => PathBuf::from(".projectlife-location.json"),
    }
}

pub fn save_location(root: &Path) -> std::io::Result<()> {
    let p = location_file();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = format!("{{\"archiveRoot\":{}}}\n", Value::String(root.to_string_lossy().to_string()));
    util::write_atomic(&p, text.as_bytes())
}

pub fn read_location() -> Option<PathBuf> {
    let text = fs::read_to_string(location_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("archiveRoot").and_then(|x| x.as_str()).map(PathBuf::from)
}

/// Resolve the archive root: explicit `--archive`, then the environment, then the location file.
pub fn resolve_archive_root(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    if let Ok(env) = std::env::var("PROJECTLIFE_ARCHIVE") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    read_location().ok_or_else(|| {
        "no archive configured: run `projectlife init-archive <path>` or pass --archive".to_string()
    })
}

pub fn read_file_bytes(p: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(p)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

pub const RECOVERY_TEXT: &str = r#"Project Life — recovering without the program
==============================================

The archive is ordinary files. No program is needed to get a version of a file back.

1. Open the journal: projects/<project-id>/events/YYYY-MM.jsonl
   Each line is one event (JSON). Find the last "put" event for the path you want whose
   "ts" field is not later than the moment you need.
2. Take its "hash" field and copy blobs/<first 2 chars>/<next 2 chars>/<hash> under the
   name you want.
3. Verify the file is intact: sha256sum <file> must equal the file's own name.
4. With Python 3 available the script next to this file does the same:
   python3 recover.py --help

Useful to know:
  * "delete" — the file disappeared; take the previous "put" for that path.
  * "move"   — contents moved from "from" to "to".
  * "gap"    — the program was not observing during that interval; states inside it are absent.
  * "snapshot" with reason "prune-anchor" — the start of the available history after pruning.
  * Format and rules: STORAGE_FORMAT.md
"#;
