//! The filesystem notification trigger.
//!
//! What this module is, and what it deliberately is not:
//!
//! * It is a **trigger**. It decides *when* the next observation cycle starts and nothing else. The
//!   cycle it starts is the ordinary full pass — the same code path as the periodic one — so a
//!   wrong, missing or duplicated notification can never change what is stored or skip a file.
//!   (FR-WCH-4)
//! * It is **not** the promise. The periodic full pass stays mandatory: a notification never
//!   postpones it, so the longest a change can stay unseen is still bounded by the interval. What
//!   notifications buy is *latency*: the version appears after ~debounce instead of after up to one
//!   interval.
//! * It never watches a directory the filters exclude. `node_modules` is named, not opened — the
//!   same rule as the walk, and the reason the watch count stays proportional to the source tree.
//!
//! Backends: Linux `inotify` (implemented and measured here). On macOS and Windows this build
//! reports itself unavailable instead of pretending, and the daemon falls back to the periodic pass
//! with a log line saying so.
//!
//! Two deliberate fault injectors exist for the tests (unset in normal use):
//! `PROJECTLIFE_DROP_EVENTS=1` reads every notification and throws it away — the run then sees
//! exactly what a run with lost notifications sees; `PROJECTLIFE_DRAIN_DELAY_MS=<ms>` refuses to
//! drain the kernel queue for the first N milliseconds, so the queue really can overflow.

use crate::archive::Archive;
use crate::filters::FilterConfig;
use crate::glob::IgnoreRules;
use crate::scan::open_ignore_rules;
use crate::util;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One watched project root together with the filter in force for it, so an event can be judged by
/// exactly the same rules the walk uses.
struct Root {
    name: String,
    path: PathBuf,
    filter: FilterConfig,
    ignore: IgnoreRules,
    gitignore: IgnoreRules,
    archive_prefixes: Vec<PathBuf>,
}

/// Why the trigger is running, or why it is not.
pub enum Mode {
    /// A real backend is installed.
    Active,
    /// Turned off by configuration (`watchTriggers=false`).
    Off,
    /// No usable backend on this platform, or the kernel's watch limit was reached.
    Unavailable(String),
}

pub struct Watcher {
    mode: Mode,
    roots: Vec<Root>,
    /// watch descriptor -> (root index, relative directory)
    wd_path: BTreeMap<i32, (usize, String)>,
    path_wd: BTreeMap<(usize, String), i32>,
    pending: BTreeSet<String>,
    /// Telemetry, written to `watch_state.json` and printed by `daemon status`.
    pub events: u64,
    pub trigger_cycles: u64,
    pub periodic_cycles: u64,
    /// Round 293: how many of `trigger_cycles` were partial passes (a walk restricted to the
    /// notification paths). `trigger_cycles - partial_cycles` were notification-driven full passes.
    pub partial_cycles: u64,
    /// What the last partial pass did, for `watch_state.json`.
    pub last_partial: serde_json::Value,
    pub overflows: u64,
    pub lost: u64,
    pub refusals: u64,
    pub dirs_watched: usize,
    pub last_event_ms: i64,
    pub last_changed: Vec<String>,
    drop_events: bool,
    drain_after_ms: i64,
    started_ms: i64,
    #[cfg(target_os = "linux")]
    fd: i32,
    /// Windows: watch id -> the change-notification handle, and the project root that handle belongs
    /// to. `isize` rather than a pointer type on purpose: the struct stays `Send`, and nothing
    /// outside this module has to know what a HANDLE is.
    #[cfg(windows)]
    w_handles: BTreeMap<i32, isize>,
    #[cfg(windows)]
    w_root: BTreeMap<i32, PathBuf>,
    #[cfg(windows)]
    w_next: i32,
    /// The system's own words for the last handle that could not be opened.
    #[cfg(windows)]
    w_refusal: Option<String>,
}

impl Watcher {
    fn empty(drop_events: bool, drain_after_ms: i64) -> Watcher {
        Watcher {
            mode: Mode::Off,
            roots: Vec::new(),
            wd_path: BTreeMap::new(),
            path_wd: BTreeMap::new(),
            pending: BTreeSet::new(),
            events: 0,
            trigger_cycles: 0,
            periodic_cycles: 0,
            partial_cycles: 0,
            last_partial: serde_json::Value::Null,
            overflows: 0,
            lost: 0,
            refusals: 0,
            dirs_watched: 0,
            last_event_ms: 0,
            last_changed: Vec::new(),
            drop_events,
            drain_after_ms,
            started_ms: util::now_ms(),
            #[cfg(target_os = "linux")]
            fd: -1,
            #[cfg(windows)]
            w_handles: BTreeMap::new(),
            #[cfg(windows)]
            w_root: BTreeMap::new(),
            #[cfg(windows)]
            w_next: 1,
            #[cfg(windows)]
            w_refusal: None,
        }
    }

    /// Install the trigger. Never fails: an unusable backend is reported by `describe()` and the
    /// daemon keeps working on the periodic pass alone.
    pub fn install(enable: bool) -> Watcher {
        let drop_events = std::env::var("PROJECTLIFE_DROP_EVENTS").map(|v| v != "0").unwrap_or(false);
        let drain_after_ms = std::env::var("PROJECTLIFE_DRAIN_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if !enable {
            return Watcher::empty(drop_events, drain_after_ms);
        }
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if fd < 0 {
                let e = std::io::Error::last_os_error();
                let mut w = Watcher::empty(drop_events, drain_after_ms);
                w.mode = Mode::Unavailable(format!("inotify_init1 failed: {e}"));
                return w;
            }
            let mut w = Watcher::empty(drop_events, drain_after_ms);
            w.fd = fd;
            w.mode = Mode::Active;
            w
        }
        #[cfg(windows)]
        {
            // Windows has no inotify; it has `FindFirstChangeNotificationW`, one handle per watched
            // root, recursive. There is no handle to open yet — `refresh()` opens one per project —
            // so this only declares that a backend exists. If every root then refuses, `add()` turns
            // this into `Unavailable` with the system's own words, and the periodic pass carries the
            // promise alone (exactly as on a machine whose watch limit is reached).
            let mut w = Watcher::empty(drop_events, drain_after_ms);
            w.mode = Mode::Active;
            w
        }
        // macOS in this build: no backend, as in round 300 — the periodic pass is the trigger there
        // and `docs/LIMITATIONS.md` says so. Changing that means FSEvents, which is a round of its
        // own and not something to smuggle in beside the Windows work.
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let mut w = Watcher::empty(drop_events, drain_after_ms);
            w.mode = Mode::Unavailable(format!(
                "no notification backend for {} in this build (Linux inotify, Windows change notifications)",
                std::env::consts::OS
            ));
            w
        }
    }

    pub fn active(&self) -> bool {
        matches!(self.mode, Mode::Active)
    }

    /// Round 300 (FR-CFG-3): turn the notification backend on or off while a daemon is running.
    /// The counters travel across the change, so `watch_state.json` does not lose the history of what
    /// happened before it. Returns true when something actually changed.
    pub fn set_enabled(&mut self, enable: bool) -> bool {
        if enable == self.active() {
            return false;
        }
        let keep = (
            self.events,
            self.trigger_cycles,
            self.periodic_cycles,
            self.partial_cycles,
            self.overflows,
            self.lost,
            self.refusals,
            self.last_event_ms,
            self.started_ms,
            self.drop_events,
            self.drain_after_ms,
        );
        let keep_last = std::mem::take(&mut self.last_changed);
        let keep_pending = std::mem::take(&mut self.pending);
        let keep_partial = self.last_partial.take();
        *self = Watcher::install(enable);
        self.events = keep.0;
        self.trigger_cycles = keep.1;
        self.periodic_cycles = keep.2;
        self.partial_cycles = keep.3;
        self.overflows = keep.4;
        self.lost = keep.5;
        self.refusals = keep.6;
        self.last_event_ms = keep.7;
        self.started_ms = keep.8;
        self.drop_events = keep.9;
        self.drain_after_ms = keep.10;
        self.last_changed = keep_last;
        self.pending = keep_pending;
        self.last_partial = keep_partial;
        true
    }

    /// A one-line description for the log and for `daemon status`.
    pub fn describe(&self) -> String {
        match &self.mode {
            // Each platform says what it really watches. inotify watches directories and can name
            // the one that changed; a Windows change notification covers a whole project root, so on
            // Windows "which directory" is not known and the words do not pretend otherwise.
            #[cfg(target_os = "linux")]
            Mode::Active => format!("inotify ({} directories watched)", self.dirs_watched),
            #[cfg(windows)]
            Mode::Active => format!(
                "Windows change notifications ({} project root(s) watched recursively{})",
                self.w_handles.len(),
                match &self.w_refusal {
                    Some(e) => format!("; last refusal: {e}"),
                    None => String::new(),
                }
            ),
            #[cfg(not(any(target_os = "linux", windows)))]
            Mode::Active => format!("notifications ({} directories watched)", self.dirs_watched),
            Mode::Off => "off (watchTriggers=false)".into(),
            Mode::Unavailable(why) => format!("unavailable — {why}"),
        }
    }

    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            #[cfg(target_os = "linux")]
            Mode::Active => "inotify",
            #[cfg(windows)]
            Mode::Active => "win-notify",
            #[cfg(not(any(target_os = "linux", windows)))]
            Mode::Active => "unavailable",
            Mode::Off => "off",
            Mode::Unavailable(_) => "unavailable",
        }
    }

    /// Rebuild the watch set for every project the archive observes. Idempotent, and safe to call
    /// at any time: it adds what is missing, drops what is gone. This is the self-healing path — a
    /// watch lost to a queue overflow or to a directory the kernel silently dropped comes back here
    /// rather than staying lost.
    pub fn refresh(&mut self, archive: &Archive) -> Result<usize, String> {
        if !self.active() {
            return Ok(0);
        }
        let mut roots: Vec<Root> = Vec::new();
        let prefixes = vec![archive.root.clone()];
        for project in archive.load_projects()? {
            if !observed(&project.state()) {
                continue;
            }
            let path = project.project_path();
            if !path.is_dir() {
                continue;
            }
            let (own, git) = open_ignore_rules(&path);
            roots.push(Root {
                name: project.name.clone(),
                filter: FilterConfig::for_profile(&project.profile(), &project.settings()),
                ignore: own,
                gitignore: git,
                archive_prefixes: prefixes.clone(),
                path,
            });
        }
        self.roots = roots;
        let wanted: BTreeSet<(usize, String)> = self
            .roots
            .iter()
            .enumerate()
            .flat_map(|(i, r)| collect_dirs(r).into_iter().map(move |rel| (i, rel)))
            .collect();
        let stale: Vec<(usize, String)> = self.path_wd.keys().filter(|k| !wanted.contains(*k)).cloned().collect();
        for key in stale {
            if let Some(wd) = self.path_wd.remove(&key) {
                self.wd_path.remove(&wd);
                self.rm(wd);
            }
        }
        for key in &wanted {
            if !self.path_wd.contains_key(key) {
                let (i, rel) = key.clone();
                self.add(i, &rel);
            }
        }
        self.dirs_watched = self.path_wd.len();
        Ok(self.dirs_watched)
    }

    /// Wait up to `timeout_ms` for the notification channel to become readable.
    ///
    /// Returns true when there is something to read. With no backend this is an ordinary sleep, so
    /// the periodic pass keeps its rhythm (chopped into 200 ms slices so a stop signal is noticed).
    pub fn wait(&mut self, timeout_ms: i64) -> bool {
        if timeout_ms <= 0 {
            return false;
        }
        #[cfg(windows)]
        if self.active() && !self.w_handles.is_empty() {
            // The same discipline as the Linux branch: wait no longer than a second at a time, so a
            // stop request or a configuration change is noticed promptly.
            let hs: Vec<isize> = self.w_handles.values().cloned().collect();
            let t = timeout_ms.min(1000).max(1) as u32;
            return winwatch::wait_any(&hs, t);
        }
        #[cfg(target_os = "linux")]
        if self.active() {
            let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
            let t = timeout_ms.min(1000) as libc::c_int;
            let r = unsafe { libc::poll(&mut pfd, 1, t) };
            return r > 0 && (pfd.revents & libc::POLLIN) != 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(timeout_ms.min(200).max(1) as u64));
        false
    }

    /// Read everything pending and remember the paths that changed. Returns how many *new* paths
    /// entered the pending set; 0 with an active watcher means nothing arrived.
    pub fn collect(&mut self) -> usize {
        if !self.active() {
            return 0;
        }
        if self.drain_after_ms > 0 && util::now_ms() - self.started_ms < self.drain_after_ms {
            // Deliberate not-draining (test hook): the kernel queue is left to fill up.
            return 0;
        }
        let before = self.pending.len();
        let (paths, overflow) = self.read_events();
        if self.drop_events {
            // The injected fault: the notifications really did arrive and this run throws them
            // away. Whatever is stored after this must have been found by the periodic pass.
            self.lost += paths.len() as u64;
            return 0;
        }
        if overflow {
            // The queue overflowed: notifications WERE lost. Saying so is the point — the caller
            // starts a full pass at once, which is the whole reason the periodic pass is not
            // optional.
            self.overflows += 1;
            self.pending.insert("<queue overflow: notifications were lost>".into());
        }
        if !paths.is_empty() {
            self.events += paths.len() as u64;
            self.last_event_ms = util::now_ms();
            for p in paths {
                self.pending.insert(p);
            }
        }
        self.pending.len().saturating_sub(before)
    }

    /// Take the pending paths (the ones a cycle should be started for) and clear them.
    pub fn take_pending(&mut self) -> Vec<String> {
        let v: Vec<String> = self.pending.iter().cloned().collect();
        self.pending.clear();
        if !v.is_empty() {
            self.last_changed = v.iter().take(20).cloned().collect();
            self.last_event_ms = util::now_ms();
        }
        v
    }

    /// Remember what the last partial pass did, for `watch_state.json`.
    pub fn set_last_partial(&mut self, v: serde_json::Value) {
        self.last_partial = v;
    }

    /// Write the trigger state next to the heartbeat, so what the trigger is doing is measurable
    /// from outside the process (`daemon status`, the acceptance tests, `watch_latency.py`).
    pub fn write_state(&self, archive: &Archive) {
        let changed: Vec<serde_json::Value> =
            self.last_changed.iter().map(|p| serde_json::Value::from(p.clone())).collect();
        let doc = serde_json::json!({
            "schemaVersion": 1,
            "mode": self.mode_name(),
            "describe": self.describe(),
            "pid": std::process::id(),
            "watchedDirs": self.dirs_watched,
            "events": self.events,
            "triggerCycles": self.trigger_cycles,
            "periodicCycles": self.periodic_cycles,
            "partialCycles": self.partial_cycles,
            "overflows": self.overflows,
            "lostEvents": self.lost,
            "watchRefusals": self.refusals,
            "pending": self.pending.len(),
            "lastEventAt": if self.last_event_ms > 0 { crate::archive::iso_ms(self.last_event_ms) } else { String::new() },
            "lastChangedPaths": changed,
            "lastPartial": self.last_partial,
            "updatedAt": crate::archive::iso_ms(util::now_ms()),
        });
        let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
        let _ = util::write_atomic(&archive.root.join("watch_state.json"), text.as_bytes());
    }

    // ---------------------------------------------------------------------------------------
    // Linux inotify plumbing
    // ---------------------------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    fn watch_mask() -> u32 {
        libc::IN_CREATE
            | libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_ONLYDIR
    }

    #[cfg(target_os = "linux")]
    fn add(&mut self, root_idx: usize, rel: &str) {
        if !self.active() {
            return;
        }
        use std::os::unix::ffi::OsStrExt;
        let abs = join_rel(&self.roots[root_idx].path, rel);
        let c = match std::ffi::CString::new(abs.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => return,
        };
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), Self::watch_mask()) };
        if wd < 0 {
            let e = std::io::Error::last_os_error();
            self.refusals += 1;
            if e.raw_os_error() == Some(libc::ENOSPC) {
                // The kernel's watch limit: stop pretending and let the periodic pass carry the
                // promise. A stated limitation, not a silent failure.
                let n = self.path_wd.len();
                self.mode = Mode::Unavailable(format!(
                    "the kernel's inotify watch limit is reached (ENOSPC) after {n} directories — \
                     raise fs.inotify.max_user_watches, or leave the periodic pass as the only trigger"
                ));
            }
            return;
        }
        self.wd_path.insert(wd, (root_idx, rel.to_string()));
        self.path_wd.insert((root_idx, rel.to_string()), wd);
    }

    /// Windows: one notification handle per project root, and only per root.
    ///
    /// `WaitForMultipleObjects` waits on at most 64 handles, so the Linux shape — one watch per
    /// directory — is not available here; and because a change notification says "something under
    /// this root changed" and not which file, the handle is opened for the root with
    /// `bWatchSubtree = TRUE`. A `rel` other than the root is deliberately not watched: pretending
    /// to have per-directory granularity would make `describe()` lie.
    #[cfg(windows)]
    fn add(&mut self, root_idx: usize, rel: &str) {
        if !self.active() || !rel.is_empty() {
            return;
        }
        let abs = self.roots[root_idx].path.clone();
        match winwatch::open(&abs) {
            Ok(h) => {
                let wd = self.w_next;
                self.w_next += 1;
                self.w_handles.insert(wd, h);
                self.w_root.insert(wd, abs);
                self.wd_path.insert(wd, (root_idx, String::new()));
                self.path_wd.insert((root_idx, String::new()), wd);
            }
            Err(e) => {
                self.refusals += 1;
                self.w_refusal = Some(format!("{}: {e}", self.roots[root_idx].path.display()));
                if self.w_handles.is_empty() {
                    self.mode = Mode::Unavailable(format!(
                        "the filesystem refused a change notification for {} — {e}",
                        self.roots[root_idx].path.display()
                    ));
                }
            }
        }
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn add(&mut self, _root_idx: usize, _rel: &str) {}

    #[cfg(target_os = "linux")]
    fn rm(&mut self, wd: i32) {
        if self.active() && wd >= 0 {
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
    }

    #[cfg(windows)]
    fn rm(&mut self, wd: i32) {
        if let Some(h) = self.w_handles.remove(&wd) {
            winwatch::close(h);
        }
        self.w_root.remove(&wd);
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn rm(&mut self, _wd: i32) {}

    /// Forget (and stop) every watch at or below `rel` in root `root_idx`. Used when a directory is
    /// deleted or moved away: the kernel follows the inode, so the watch has to be taken down here.
#[cfg(target_os = "linux")]
    fn drop_tree(&mut self, root_idx: usize, rel: &str) {
        let prefix = format!("{rel}/");
        let victims: Vec<(usize, String)> = self
            .path_wd
            .keys()
            .filter(|(r, p)| {
                // An empty `rel` means the root itself: only the root entry is dropped then.
                if rel.is_empty() {
                    *r == root_idx && p.is_empty()
                } else {
                    *r == root_idx && (p == rel || p.starts_with(&prefix))
                }
            })
            .cloned()
            .collect();
        for key in victims {
            if let Some(wd) = self.path_wd.remove(&key) {
                self.wd_path.remove(&wd);
                self.rm(wd);
            }
        }
    }

    /// Windows: which roots have something new, and re-arm each one.
    ///
    /// Two things this does not do, on purpose. It never reports an overflow (Windows' change
    /// notifications do not lose events the way the inotify queue does — they coalesce), and it
    /// never claims a *file*: the label it produces is the project root, which makes the pass a
    /// project-scoped pass. The periodic pass is untouched by any of this.
    #[cfg(windows)]
    fn read_events(&mut self) -> (Vec<String>, bool) {
        let mut out: BTreeSet<String> = BTreeSet::new();
        let ids: Vec<i32> = self.w_handles.keys().cloned().collect();
        for wd in ids {
            let h = match self.w_handles.get(&wd) {
                Some(h) => *h,
                None => continue,
            };
            if !winwatch::signalled(h) {
                continue;
            }
            // Re-arm before anything else: the next change must not be missed while this one is
            // being turned into work.
            if let Err(e) = winwatch::rearm(h) {
                self.refusals += 1;
                self.w_refusal = Some(e);
                continue;
            }
            let entry = match self.wd_path.get(&wd).cloned() {
                Some(e) => e,
                None => continue,
            };
            let root_idx = entry.0;
            // Self-healing against a rebuilt root list: the index a handle was opened for can mean a
            // different project after a project was removed (the same index-shift the watch maps have
            // everywhere). Rather than report a change for the wrong project, reopen the handle for
            // the root the index means now.
            let want = match self.roots.get(root_idx) {
                Some(r) => r.path.clone(),
                None => continue,
            };
            let have = self.w_root.get(&wd).cloned();
            if have.as_ref() != Some(&want) {
                winwatch::close(h);
                let new_handle = winwatch::open(&want);
                self.w_handles.remove(&wd);
                self.w_root.remove(&wd);
                self.wd_path.remove(&wd);
                self.path_wd.remove(&(root_idx, String::new()));
                match new_handle {
                    Ok(nh) => {
                        self.w_handles.insert(wd, nh);
                        self.w_root.insert(wd, want.clone());
                        self.wd_path.insert(wd, (root_idx, String::new()));
                        self.path_wd.insert((root_idx, String::new()), wd);
                    }
                    Err(e) => {
                        self.refusals += 1;
                        self.w_refusal = Some(format!("{}: {e}", want.display()));
                    }
                }
                continue;
            }
            out.insert(self.label(root_idx, ""));
        }
        (out.into_iter().collect(), false)
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn read_events(&mut self) -> (Vec<String>, bool) {
        (Vec::new(), false)
    }

    #[cfg(target_os = "linux")]
    fn read_events(&mut self) -> (Vec<String>, bool) {
        #[repr(C, align(8))]
        struct Buf([u8; 16 * 1024]);
        let mut out: BTreeSet<String> = BTreeSet::new();
        let mut buf = Buf([0u8; 16 * 1024]);
        let mut new_dirs: Vec<(usize, String)> = Vec::new();
        let mut gone_dirs: Vec<(usize, String)> = Vec::new();
        let mut overflow = false;
        let ev_size = std::mem::size_of::<libc::inotify_event>();
        loop {
            let n = unsafe { libc::read(self.fd, buf.0.as_mut_ptr() as *mut libc::c_void, buf.0.len()) };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            let base = buf.0.as_ptr();
            while off + ev_size <= n as usize {
                let ev = unsafe { &*(base.add(off) as *const libc::inotify_event) };
                let name_len = ev.len as usize;
                let name = if name_len > 0 {
                    let start = off + ev_size;
                    let end = (start + name_len).min(buf.0.len());
                    let bytes = &buf.0[start..end];
                    let stop = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                    String::from_utf8_lossy(&bytes[..stop]).to_string()
                } else {
                    String::new()
                };
                off += ev_size + name_len;
                if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                    overflow = true;
                    continue;
                }
                if ev.mask & libc::IN_IGNORED != 0 {
                    self.wd_path.remove(&ev.wd);
                    let dead: Option<(usize, String)> =
                        self.path_wd.iter().find(|(_, w)| **w == ev.wd).map(|(k, _)| k.clone());
                    if let Some(k) = dead {
                        self.path_wd.remove(&k);
                    }
                    continue;
                }
                let Some((root_idx, rel_dir)) = self.wd_path.get(&ev.wd).cloned() else {
                    continue;
                };
                let rel = if name.is_empty() {
                    rel_dir.clone()
                } else if rel_dir.is_empty() {
                    name.clone()
                } else {
                    format!("{rel_dir}/{name}")
                };
                let is_dir = ev.mask & libc::IN_ISDIR != 0;
                if is_dir {
                    if ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                        new_dirs.push((root_idx, rel.clone()));
                    } else if ev.mask
                        & (libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF)
                        != 0
                    {
                        gone_dirs.push((root_idx, rel.clone()));
                    }
                    out.insert(self.label(root_idx, &rel));
                    continue;
                }
                // A file that appeared or disappeared, or a file the filters track.
                if ev.mask & (libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_CREATE | libc::IN_MOVED_TO) != 0
                    || self.tracked(root_idx, &rel)
                {
                    out.insert(self.label(root_idx, &rel));
                }
            }
        }
        for (root_idx, rel) in gone_dirs {
            self.drop_tree(root_idx, &rel);
        }
        for (root_idx, rel) in new_dirs {
            // A directory that just appeared: watch it and everything under it, but only if the
            // filters would let the walk descend into it. The directory itself is added first —
            // files created inside it a moment later must be seen, which is what the acceptance
            // test for a brand-new folder checks.
            if self.dir_tracked(root_idx, &rel) {
                if !self.path_wd.contains_key(&(root_idx, rel.clone())) {
                    self.add(root_idx, &rel);
                }
                for d in collect_dirs_below(&self.roots[root_idx], &rel) {
                    if !self.path_wd.contains_key(&(root_idx, d.clone())) {
                        self.add(root_idx, &d);
                    }
                }
            }
        }
        self.dirs_watched = self.path_wd.len();
        (out.into_iter().collect(), overflow)
    }

    /// `project:relative/path` — the name used in the log, in `watch_state.json` and in the queue.
    fn label(&self, root_idx: usize, rel: &str) -> String {
        format!("{}:{}", self.roots[root_idx].name, rel)
    }

#[cfg(target_os = "linux")]
    /// The same decision the walk makes for a file (size 0, so the size rule — which the cycle
    /// applies with the real size — cannot decide here).
    fn tracked(&self, root_idx: usize, rel: &str) -> bool {
        let r = &self.roots[root_idx];
        r.filter.decide_file(rel, 0, &r.ignore, &r.gitignore).track
    }

#[cfg(target_os = "linux")]
    fn dir_tracked(&self, root_idx: usize, rel: &str) -> bool {
        let r = &self.roots[root_idx];
        let ap = canon_prefixes(r);
        r.filter.decide_dir(rel, &ap, &r.ignore, &r.gitignore).track
    }
}

impl Drop for Watcher {
    #[cfg(target_os = "linux")]
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
    #[cfg(not(target_os = "linux"))]
    fn drop(&mut self) {}
}

fn observed(state: &str) -> bool {
    state != "paused" && state != "removed" && state != "error"
}

fn canon_prefixes(r: &Root) -> Vec<String> {
    r.archive_prefixes
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()).to_string_lossy().replace('\\', "/"))
        .collect()
}

/// Every directory of a root the walk would descend into, including the root itself.
fn collect_dirs(r: &Root) -> Vec<String> {
    let mut out = vec![String::new()];
    out.extend(collect_dirs_below(r, ""));
    out
}

fn collect_dirs_below(r: &Root, start_rel: &str) -> Vec<String> {
    let ap = canon_prefixes(r);
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(join_rel(&r.path, start_rel), start_rel.to_string())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // The symlink rule of the walk: a link is never descended into (and IN_ONLYDIR would
            // refuse the watch anyway).
            let md = match std::fs::symlink_metadata(e.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !md.is_dir() {
                continue;
            }
            let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if !r.filter.decide_dir(&child_rel, &ap, &r.ignore, &r.gitignore).track {
                continue;
            }
            out.push(child_rel.clone());
            stack.push((e.path(), child_rel));
        }
    }
    out
}

fn join_rel(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

// ---------------------------------------------------------------------------------------------
// Windows change notifications
//
// `FindFirstChangeNotificationW` opens a handle that becomes signalled when anything under a
// directory (with `bWatchSubtree`, the whole tree below it) changes; `WaitForMultipleObjects` waits
// on up to 64 of them at once, and `FindNextChangeNotification` re-arms one after it fired. That is
// the whole backend: no overlapped I/O thread, no callback, nothing that can outlive the process.
//
// Every call here carries its real name and signature, so the compiler checks the call rather than
// my memory of it. None of it has been executed: there is no Windows machine in this round
// (`docs/PLATFORMS.md` names the one command that exercises it on a machine that has one).
// ---------------------------------------------------------------------------------------------
#[cfg(windows)]
mod winwatch {
    use std::path::Path;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FindFirstChangeNotificationW(path: *const u16, watch_subtree: i32, filter: u32) -> isize;
        fn FindNextChangeNotification(h: isize) -> i32;
        fn FindCloseChangeNotification(h: isize) -> i32;
        fn WaitForMultipleObjects(count: u32, handles: *const isize, wait_all: i32, ms: u32) -> u32;
        fn WaitForSingleObject(h: isize, ms: u32) -> u32;
    }

    /// `INVALID_HANDLE_VALUE`, as a number, for the same reason the handles are numbers.
    const INVALID_HANDLE: isize = -1;
    /// `WAIT_OBJECT_0` and `WAIT_TIMEOUT`.
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    /// What a file-history program has to be told about: names appearing and disappearing (including
    /// renames, which are a delete plus a create), sizes, last-write times, attributes, and whole
    /// directories being created. Security descriptors are deliberately not included: they are not
    /// part of the promise, and asking for them makes a refused handle much likelier.
    const FILTER: u32 = 0x0000_0001 // FILE_NOTIFY_CHANGE_FILE_NAME
        | 0x0000_0002 // FILE_NOTIFY_CHANGE_DIR_NAME
        | 0x0000_0004 // FILE_NOTIFY_CHANGE_ATTRIBUTES
        | 0x0000_0008 // FILE_NOTIFY_CHANGE_SIZE
        | 0x0000_0010 // FILE_NOTIFY_CHANGE_LAST_WRITE
        | 0x0000_0040; // FILE_NOTIFY_CHANGE_CREATION

    fn wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        let mut w: Vec<u16> = path.as_os_str().encode_wide().collect();
        w.push(0);
        w
    }

    /// Open one recursive notification for `path`. The system's own words come back on failure.
    pub fn open(path: &Path) -> Result<isize, String> {
        let w = wide(path);
        let h = unsafe { FindFirstChangeNotificationW(w.as_ptr(), 1, FILTER) };
        if h == 0 || h == INVALID_HANDLE {
            return Err(format!("FindFirstChangeNotificationW: {}", std::io::Error::last_os_error()));
        }
        Ok(h)
    }

    /// Re-arm after a signal: without this the handle never fires again.
    pub fn rearm(h: isize) -> Result<(), String> {
        if unsafe { FindNextChangeNotification(h) } == 0 {
            return Err(format!("FindNextChangeNotification: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn close(h: isize) {
        unsafe { FindCloseChangeNotification(h) };
    }

    /// Has this handle fired? A zero-second wait, which is the only way to ask.
    pub fn signalled(h: isize) -> bool {
        unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 }
    }

    /// Wait until any of these handles fires, or until the timeout runs out. Returns true when one
    /// did. Windows refuses more than 64 handles at once — `MAXIMUM_WAIT_OBJECTS` — and that is the
    /// one hard limit of this backend: the caller watches one handle per project, so the limit is
    /// 64 projects, which `describe()` reports rather than hides.
    pub fn wait_any(handles: &[isize], ms: u32) -> bool {
        let n = handles.len().min(64) as u32;
        if n == 0 {
            return false;
        }
        match unsafe { WaitForMultipleObjects(n, handles.as_ptr(), 0, ms) } {
            WAIT_TIMEOUT => false,
            // WAIT_OBJECT_0 .. WAIT_OBJECT_0 + n - 1 is a signalled handle; WAIT_FAILED (0xFFFFFFFF)
            // and WAIT_ABANDONED are not, and a false answer there costs one wait, not a lost event:
            // the handle keeps its signalled state until `FindNextChangeNotification` re-arms it.
            0xFFFF_FFFF => false,
            _ => true,
        }
    }
}
