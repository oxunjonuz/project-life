//! Round 301 — the Windows and Linux applications, and the core two things they needed.
//!
//! What can be tested *here* is everything except the Windows shell's own behaviour (there is no
//! Windows machine in this round) and the Linux shell's window (that is `app/linux` plus
//! `tools/linux_shell_check.py`, which runs the real GTK window under Xvfb). What is tested here:
//!
//! 1. **The stop request** — the one way to stop a daemon that every platform has, including the one
//!    with no signals. A running daemon honours a request that names it; a request that names somebody
//!    else cannot stop it; the request is consumed exactly once.
//! 2. **`daemon stop`** asks by name, waits for the process to actually leave, and says which of the
//!    two happened. This is the path the desktop app uses to stop the observation it started.
//! 3. **The Windows trigger shape** — a change notification names a project root, not a file
//!    (`<project>:` with an empty relative path). That label must be a *scoped pass over the root*,
//!    not an unreadable label, and it must really record the change.
//! 4. **Paths from the other platform** — `src\b.txt` and `src/b.txt` must address the same file, so
//!    an archive written on one platform is readable on the other.
//! 5. **`pid_alive`** really answers about a pid that is gone, which is what lets a lock be taken
//!    over after a crash — the Windows implementation of it cannot be run here, but the Linux one and
//!    the callers can, and the contract is the same on both.

use projectlife::archive::Archive;
use projectlife::daemon;
use projectlife::watch::Watcher;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn core() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    let candidate = p.join("projectlife");
    if candidate.exists() {
        return candidate;
    }
    PathBuf::from(env!("CARGO_BIN_EXE_projectlife"))
}

fn tmp(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("pl301-{name}-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(archive: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(core())
        .env("PROJECTLIFE_HOME", home)
        .arg("--archive")
        .arg(archive)
        .args(args)
        .output()
        .expect("run projectlife");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    archive: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = tmp(name);
        let work = root.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("a.txt"), "alpha\n").unwrap();
        fs::write(work.join("src/b.txt"), "beta\n").unwrap();
        fs::write(work.join("src/c.txt"), "gamma\n").unwrap();
        let f = Fixture { work, archive: root.join("archive"), home: root.join("home"), root };
        fs::create_dir_all(&f.home).unwrap();
        let (c, _, e) = run(&f.archive, &f.home, &["init-archive", f.archive.to_str().unwrap()]);
        assert_eq!(c, 0, "init-archive: {e}");
        // No space warnings in a test: the rules stay, the thresholds are moved out of the way.
        for key in ["stopFreePercent", "warnFreePercent"] {
            let (c, _, e) = run(&f.archive, &f.home, &["config", "set", key, "0"]);
            assert_eq!(c, 0, "config set {key}: {e}");
        }
        let (c, o, e) = run(&f.archive, &f.home, &["add", f.work.to_str().unwrap(), "--profile", "all", "--yes"]);
        assert_eq!(c, 0, "add: {o}{e}");
        f
    }

    fn log(&self) -> String {
        fs::read_to_string(self.archive.join("logs/projectlife.log")).unwrap_or_default()
    }

    fn projects(&self) -> String {
        let (c, o, e) = run(&self.archive, &self.home, &["list", "--json"]);
        assert_eq!(c, 0, "list: {o}{e}");
        o
    }
}

/// A daemon started by a test, stopped when the test ends — including when it panics.
struct DaemonGuard(std::process::Child);

impl DaemonGuard {
    fn start(f: &Fixture) -> DaemonGuard {
        let child = Command::new(core())
            .env("PROJECTLIFE_HOME", &f.home)
            .arg("--archive")
            .arg(&f.archive)
            .args(["daemon", "run"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the daemon must start");
        let g = DaemonGuard(child);
        assert!(g.wait_for_log(f, "daemon started", 100), "the daemon never said it started");
        g
    }

    fn wait_for_log(&self, f: &Fixture, needle: &str, ticks: usize) -> bool {
        for _ in 0..ticks {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if f.log().contains(needle) {
                return true;
            }
        }
        false
    }

    fn pid(&self) -> i32 {
        self.0.id() as i32
    }

    fn exited(&mut self, ticks: usize) -> bool {
        for _ in 0..ticks {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = Command::new("kill").arg("-TERM").arg(self.pid().to_string()).status();
        for _ in 0..60 {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.0.kill();
    }
}

// ------------------------------------------------------------------------------------------
// 1. The stop request: every platform has this one, signals or not
// ------------------------------------------------------------------------------------------

#[test]
fn a_stop_request_stops_the_daemon_it_names() {
    let f = Fixture::new("stop-honoured");
    let mut d = DaemonGuard::start(&f);
    let pid = d.pid();

    let arch = Archive::open(&f.archive).unwrap();
    let p = arch.request_stop(pid).expect("the request is written");
    assert!(p.ends_with("stop.request"), "{}", p.display());

    assert!(d.exited(80), "the daemon did not stop within eight seconds of the request");
    assert!(
        f.log().contains("daemon stopped on request"),
        "the log must say which reason stopped it, not always \"signal\":\n{}",
        f.log()
    );
    assert!(
        f.log().contains("stop requested: the request file names this process"),
        "the daemon must say why it is leaving:\n{}",
        f.log()
    );
    assert!(!p.exists(), "the request is consumed, not left lying about");
    // The locks are released on the way out, so the next daemon starts without a `doctor --fix-lock`.
    let arch = Archive::open(&f.archive).unwrap();
    assert!(arch.daemon_holder().is_none(), "the daemon lock must be released");
}

#[test]
fn a_stop_request_that_names_somebody_else_cannot_stop_a_daemon() {
    let f = Fixture::new("stop-stale");
    // Somebody else's pid — this test process — and it is alive, so this is the dangerous case: a
    // request left behind by an earlier run must not stop the daemon that starts now.
    let other = std::process::id() as i32;
    let arch = Archive::open(&f.archive).unwrap();
    let p = arch.request_stop(other).unwrap();

    let mut d = DaemonGuard::start(&f);
    assert!(
        f.log().contains("ignoring a stop request left behind"),
        "a stale request must be named in the log, not obeyed silently:\n{}",
        f.log()
    );
    assert!(!p.exists(), "the stale request is removed so it can never bite later");
    assert!(!d.exited(20), "the daemon must still be running: a request for another process stopped it");
}

#[test]
fn a_stop_request_is_honoured_once_and_only_by_the_process_it_names() {
    let f = Fixture::new("stop-once");
    let arch = Archive::open(&f.archive).unwrap();
    let mine = std::process::id() as i32;
    let other = 424242;

    let p = arch.request_stop(other).unwrap();
    assert_eq!(arch.stop_request_holder(), Some(other));
    assert!(
        arch.take_stop_request(mine).is_none(),
        "a request for another process must not be consumed by this one"
    );
    assert!(p.exists(), "and it must still be there for the process it names");

    let line = arch.take_stop_request(other).expect("the request names this pid");
    assert!(line.starts_with(&other.to_string()), "the line carries the pid: {line}");
    assert!(!p.exists(), "honoured exactly once");
    assert!(arch.take_stop_request(other).is_none(), "and not twice");
}

#[test]
fn the_stop_command_asks_by_name_and_says_whether_it_worked() {
    let f = Fixture::new("daemon-stop-cli");
    let mut d = DaemonGuard::start(&f);
    let pid = d.pid();

    let (code, out, err) = run(&f.archive, &f.home, &["daemon", "stop"]);
    assert_eq!(code, 0, "daemon stop: {out}{err}");
    assert!(
        out.contains(&format!("stop request written for pid {pid}")),
        "the command must name who it asked: {out}"
    );
    assert!(
        out.contains(&format!("daemon (pid {pid}) stopped")),
        "and must only say \"stopped\" once it really is: {out}"
    );
    assert!(d.exited(20), "the process is gone, which is what the sentence promised");

    // With nothing running, the same command says so instead of pretending.
    let (code, out, _err) = run(&f.archive, &f.home, &["daemon", "stop"]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing to stop"), "{out}");
}

// ------------------------------------------------------------------------------------------
// 2. The Windows trigger shape: a root, not a file
// ------------------------------------------------------------------------------------------

#[test]
fn a_notification_that_names_a_whole_project_is_a_pass_over_that_root() {
    let f = Fixture::new("win-label");
    // This is exactly what the Windows backend produces: `FindFirstChangeNotificationW` reports that
    // something under a root changed, and cannot say what — so the label is `<project>:`.
    let labels = vec!["work:".to_string()];
    let (by_project, must_be_full) = daemon::group_trigger_paths(&labels);
    assert!(!must_be_full, "a root label is readable as a path; it must not force a full pass");
    assert_eq!(by_project.get("work"), Some(&vec![String::new()]));

    // Change a file two levels down, where a root-level signal would have to find it.
    fs::write(f.work.join("src/b.txt"), "beta changed\n").unwrap();
    let arch = Archive::open(&f.archive).unwrap();
    let res = daemon::run_partial_cycle(&arch, &by_project, &labels).expect("the pass runs");
    let rep = res
        .projects
        .iter()
        .find(|(n, _)| n == "work")
        .expect("the project is in the report")
        .1
        .as_ref()
        .unwrap();
    assert!(rep.partial, "a notification-driven pass is a partial pass");
    assert!(rep.changed >= 1, "the change two levels down was not recorded: {rep:?}");
    assert!(rep.blobs_new >= 1, "the new content must be stored as its own blob");
    assert!(f.projects().contains("work"), "{}", f.projects());
}

#[test]
fn notification_paths_written_with_backslashes_address_the_same_file() {
    // The same file, spelled the way the other platform spells it. If these two ever diverge, an
    // archive written on one system stops meaning the same thing on the other.
    assert_eq!(projectlife::scan::normalize_rel("src\\b.txt"), "src/b.txt");
    assert_eq!(
        projectlife::scan::normalize_rel("C:\\Users\\uc\\proj\\src\\a.ts"),
        "C:/Users/uc/proj/src/a.ts"
    );
    assert_eq!(projectlife::scan::normalize_rel("./src//b.txt"), "src/b.txt");

    let labels = vec!["work:src\\b.txt".to_string()];
    let (by_project, must_be_full) = daemon::group_trigger_paths(&labels);
    assert!(!must_be_full);
    assert_eq!(by_project.get("work"), Some(&vec!["src/b.txt".to_string()]));

    let f = Fixture::new("backslash-label");
    fs::write(f.work.join("src/b.txt"), "beta changed\n").unwrap();
    let arch = Archive::open(&f.archive).unwrap();
    let res = daemon::run_partial_cycle(&arch, &by_project, &labels).expect("the pass runs");
    let rep = res
        .projects
        .iter()
        .find(|(n, _)| n == "work")
        .expect("the project is in the report")
        .1
        .as_ref()
        .unwrap();
    assert!(rep.changed >= 1, "a backslash path must find the file: {rep:?}");
}

// ------------------------------------------------------------------------------------------
// 3. The platform layer: what each system can actually answer
// ------------------------------------------------------------------------------------------

#[test]
fn a_pid_that_is_gone_is_not_alive() {
    // The lock files decide whether their holder is gone by asking this. On Windows the same question
    // is answered with OpenProcess + GetExitCodeProcess (compiled, never executed here); the contract
    // is what has to hold, and the Linux implementation of it is what runs in this test.
    assert!(projectlife::archive::pid_alive(std::process::id() as i32), "this process is alive");

    let mut child = Command::new("/bin/sh").arg("-c").arg("exit 0").spawn().expect("spawn");
    let pid = child.id() as i32;
    let _ = child.wait();
    // A reaped child is gone; pid reuse inside this test would be the only way to see it alive.
    assert!(!projectlife::archive::pid_alive(pid), "pid {pid} was reaped and is not alive");
}

#[test]
fn the_trigger_says_what_this_platform_can_see() {
    let off = Watcher::install(false);
    assert_eq!(off.mode_name(), "off");
    assert_eq!(off.describe(), "off (watchTriggers=false)");
    assert!(!off.active());

    #[cfg(target_os = "linux")]
    {
        let live = Watcher::install(true);
        assert!(live.active(), "inotify is the Linux backend and it must still install");
        assert_eq!(live.mode_name(), "inotify", "Linux still reports inotify, unchanged by round 301");
        assert!(live.describe().contains("inotify"), "{}", live.describe());
    }

    // On Windows the same two calls answer "win-notify" and name *roots*, not directories
    // (`src/watch.rs`); that code is compiled for x86_64-pc-windows-gnu and not executed here.
    assert_ne!(std::env::consts::OS, "");
}
