//! Project Life — the desktop app's local server.
//!
//! This process is what the window talks to. It does not know how to store anything: it drives the
//! `projectlife` binary that ships beside it (observation, history, restore, export, import) and
//! reads that program's own `--json` output. The window is the only client, on 127.0.0.1, behind a
//! token generated at launch.
//!
//! Started by `ProjectLife.app`; also runnable on its own, which is how the end-to-end tests drive
//! the very same code path with a browser instead of the macOS shell.
//!
//! How it gets its socket, and what it does when it cannot, lives in `listen`. How it answers the
//! question "why could it not" lives in `diag`.

mod api;
mod assets;
mod diag;
mod http;
mod jobs;
mod listen;
mod menu;
mod pl;
mod sys;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

static STOP: AtomicBool = AtomicBool::new(false);

/// One place where this process says things. Every line carries the moment it happened, and every
/// line lands in `daemon.log` — the same file the app appends its own words to. A person looking at
/// a failure should find the reason in one place, in the process's own words, not just in a dialog.
struct Log {
    path: Option<PathBuf>,
}

impl Log {
    fn new(path: Option<PathBuf>) -> Log {
        Log { path }
    }
    fn line(&self, msg: &str) {
        let stamped = format!("{} projectlife-ui: {msg}", listen::stamp(pl::now_ms()));
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                use std::io::Write;
                let _ = writeln!(f, "{stamped}");
                return;
            }
        }
        eprintln!("{stamped}");
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut archive: Option<PathBuf> = None;
    let mut port: u16 = listen::DEFAULT_PORT;
    let mut port_range: u16 = listen::DEFAULT_RANGE;
    let mut token: Option<String> = None;
    let mut pl_bin: Option<PathBuf> = None;
    let mut home: Option<PathBuf> = std::env::var("PROJECTLIFE_HOME").ok().map(PathBuf::from);
    let mut native = false;
    let mut log_dir: Option<PathBuf> = None;
    let mut lang = "en".to_string();
    let mut ipc: Option<PathBuf> = None;
    let mut no_ipc = false;
    // An inherited descriptor (unix): `i32` is the type on every platform here, so the argument can
    // be refused with a sentence on Windows instead of failing to compile there.
    let mut listen_fd: Option<listen::ListenFd> = None;
    let mut diagnose = false;

    let mut i = 0;
    while i < argv.len() {
        let take = |i: &mut usize| -> Option<String> {
            *i += 1;
            argv.get(*i).cloned()
        };
        match argv[i].as_str() {
            "--archive" => archive = take(&mut i).map(PathBuf::from),
            "--port" => port = take(&mut i).and_then(|v| v.parse().ok()).unwrap_or(0),
            "--port-range" => port_range = take(&mut i).and_then(|v| v.parse().ok()).unwrap_or(listen::DEFAULT_RANGE),
            "--token" => token = take(&mut i),
            "--pl" => pl_bin = take(&mut i).map(PathBuf::from),
            "--home" => home = take(&mut i).map(PathBuf::from),
            "--log-dir" => log_dir = take(&mut i).map(PathBuf::from),
            "--lang" => lang = take(&mut i).unwrap_or_else(|| "en".into()),
            "--native" => native = true,
            "--ipc" => ipc = take(&mut i).map(PathBuf::from),
            "--no-ipc" => no_ipc = true,
            "--listen-fd" => {
                let v = take(&mut i).and_then(|x| x.parse::<i32>().ok());
                if v.is_some() && !sys::DESCRIPTOR_PASSING {
                    // Said out loud rather than ignored in silence: on this platform there is no
                    // descriptor passing at all, so the port ladder is the only route and a caller
                    // that passed `--listen-fd` is told so.
                    eprintln!(
                        "projectlife-ui: this platform hands no descriptor between processes; \
                         --listen-fd is ignored and the port ladder is used"
                    );
                }
                listen_fd = v;
            }
            "--diagnose" => diagnose = true,
            "--help" => {
                println!(
                    "projectlife-ui [--archive DIR] [--port N] [--port-range N] [--token T] [--pl PATH]\n\
                     \x20               [--native] [--log-dir DIR] [--ipc PATH] [--no-ipc] [--listen-fd N]\n\
                     \x20               [--lang LANG] [--diagnose]"
                );
                println!(
                    "\n  --port N         first port to try on 127.0.0.1 (default {}); 0 means the\n\
                     \x20                  kernel's own choice, and nothing else is tried",
                    listen::DEFAULT_PORT
                );
                println!("  --port-range N   how many neighbouring ports to try after it (default {})", listen::DEFAULT_RANGE);
                println!("  --ipc PATH       unix socket to ask the app for a socket on if all else fails");
                println!("  --listen-fd N    serve on an already-listening descriptor passed by the app");
                println!("  --diagnose       ask this machine what it allows, print it, write diagnose.txt, exit");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    let app_home = log_dir.clone().unwrap_or_else(app_support_dir);
    let _ = std::fs::create_dir_all(&app_home);
    let daemon_log = app_home.join("daemon.log");
    let log = Log::new(Some(daemon_log.clone()));
    // Which machine is this, and which build? A person reading `daemon.log` after a problem should
    // not have to guess either, and on Windows that line is the only place the shell's own answer
    // (RtlGetVersion) is recorded.
    log.line(&format!(
        "projectlife-ui {} starting on {} (pid {}, started by pid {}), app folder {}",
        env!("CARGO_PKG_VERSION"),
        sys::os_line(),
        sys::pid(),
        sys::parent_pid(),
        app_home.display()
    ));

    if diagnose {
        let j = diag::run(&app_home, if port == 0 { listen::DEFAULT_PORT } else { port });
        println!("{}", serde_json::to_string(&j).unwrap_or_default());
        return;
    }

    // The core binary: the one beside this executable inside the bundle, else PATH.
    let bin = pl_bin
        .or_else(|| std::env::var("PROJECTLIFE_BIN").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::current_exe().ok().and_then(|e| {
                let sib = e.parent()?.join("projectlife");
                if sib.is_file() {
                    Some(sib)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| PathBuf::from("projectlife"));
    if !bin.exists() {
        let msg = format!("the core binary was not found ({}); pass --pl <path>", bin.display());
        log.line(&msg);
        eprintln!("projectlife-ui: {msg}");
        std::process::exit(2);
    }

    let token = token.unwrap_or_else(random_token);
    let jobs = jobs::Registry::new();

    let ctx = Arc::new(api::Ctx {
        bin,
        home,
        archive: Mutex::new(archive.clone()),
        token: token.clone(),
        jobs,
        daemon: Mutex::new(None),
        daemon_log: daemon_log.clone(),
        native,
        ui_lang: Mutex::new(lang.clone()),
        self_stopped_ms: Mutex::new(None),
    });

    let ipc_path: Option<PathBuf> = if no_ipc {
        None
    } else {
        Some(ipc.clone().unwrap_or_else(|| listen::ipc_path(&app_home)))
    };

    let port_arg = port;
    let range = port_range;
    // Announced before we ask, so the app can bind while we are waiting rather than after we gave up.
    let announce_log = log.path.clone();
    let mut announce = |path: &Path, want: u16, attempts: &[listen::Attempt]| {
        let j = serde_json::json!({
            "ready": false,
            "needSocket": true,
            "sock": path.display().to_string(),
            "port": want,
            "attempts": attempts.iter().map(|a| serde_json::json!({
                "addr": a.addr, "kind": a.kind, "detail": a.detail
            })).collect::<Vec<_>>(),
        });
        println!("{j}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        if let Some(p) = announce_log.as_ref() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(
                    f,
                    "{} projectlife-ui: no local port could be opened ({} attempt(s)); asking the app for a socket on {}",
                    listen::stamp(pl::now_ms()),
                    attempts.len(),
                    path.display()
                );
            }
        }
    };

    let acquired = {
        let mut binder = listen::bind_local;
        let mut logger = |m: &str| log.line(m);
        listen::acquire(
            &mut binder,
            port_arg,
            range,
            listen_fd,
            ipc_path.as_deref(),
            &mut logger,
            &mut announce,
        )
    };

    let acquired = match acquired {
        Ok(a) => a,
        Err(refusal) => {
            // The window is told in JSON, the log in words, and the person gets both. Nothing here
            // is generic: every line is the system's own answer for a named address.
            let report = refusal.lines();
            for l in &report {
                log.line(l);
            }
            let j = serde_json::json!({"ready": false, "error": refusal.json()});
            println!("{j}");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let mut e = std::io::stderr();
            let _ = writeln!(e, "projectlife-ui: {}", refusal.last);
            let _ = writeln!(e, "projectlife-ui: log is {}", daemon_log.display());
            let _ = writeln!(e, "projectlife-ui: run `projectlife-ui --diagnose` for what this machine allows");
            std::process::exit(3);
        }
    };

    let listen::Acquired { listener, port: real_port, how, attempts, warnings } = acquired;
    for w in &warnings {
        log.line(&format!("warning: {w}"));
    }

    // The shell reads this line to learn where to point its web view.
    println!(
        "{}",
        serde_json::json!({
            "ready": true,
            "port": real_port,
            "how": how,
            "token": token,
            "url": format!("http://127.0.0.1:{real_port}/?token={token}"),
            "pid": std::process::id(),
            "native": native,
            "lang": lang,
            "daemonLog": daemon_log.to_string_lossy(),
            "core": ctx.bin.to_string_lossy(),
            "archive": archive.map(|a| a.to_string_lossy().to_string()),
            "attempts": attempts.iter().map(|a| serde_json::json!({
                "addr": a.addr, "kind": a.kind, "detail": a.detail
            })).collect::<Vec<_>>(),
        })
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();

    install_signal_handlers();

    let handler: http::Handler = {
        let ctx = Arc::clone(&ctx);
        Arc::new(move |req: &http::Request| api::route(&ctx, req))
    };

    let listener: TcpListener = listener;
    let mut idle = std::time::Duration::from_millis(5);
    while !STOP.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                idle = std::time::Duration::from_millis(5);
                let h = handler.clone();
                std::thread::spawn(move || {
                    let _ = http::handle_conn(stream, &h);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(idle);
                if idle < std::time::Duration::from_millis(120) {
                    idle += std::time::Duration::from_millis(5);
                }
            }
            Err(e) => {
                log.line(&format!("accept failed: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    // Leaving means the promise stops being kept, so the daemon this app started is stopped with it
    // and says so in its log. A daemon started elsewhere is never touched.
    let stopped = api::stop_daemon_for_exit(&ctx);
    log.line(if stopped {
        "the app stopped observation on exit"
    } else {
        "the app exited; no observation was started by it"
    });
    // The shell may already be gone (that is how "quit completely" ends: the window leaves first).
    // Writing to a closed pipe is not an error worth dying of, and it is certainly not worth a
    // panic in the log of a program whose whole job is to keep a record straight.
    {
        use std::io::Write;
        let line = serde_json::json!({"stopped": true, "daemonStopped": stopped});
        let _ = writeln!(std::io::stdout(), "{line}");
    }
}

/// Where this app keeps its own folder. The rule lives in `sys` (one place, three platforms):
/// `~/Library/Application Support/ProjectLife`, `$XDG_STATE_HOME/projectlife-app`, or
/// `%LOCALAPPDATA%\ProjectLife` — with `PROJECTLIFE_APP_HOME` overriding all three.
fn app_support_dir() -> PathBuf {
    sys::app_data_dir()
}

/// The token the window must present. From the operating system's own random source —
/// `/dev/urandom` on unix, `BCryptGenRandom` on Windows. When neither answers, the fallback is a
/// clock and this process's id, and the log says so rather than pretending the token is strong.
fn random_token() -> String {
    let mut buf = [0u8; 16];
    if !sys::random_bytes(&mut buf) {
        eprintln!("projectlife-ui: the system random source did not answer; the window token is weak");
        let t = pl::now_ms() as u128;
        buf.copy_from_slice(&t.to_le_bytes()[..16]);
        let pid = sys::pid() as u128;
        for (i, b) in pid.to_le_bytes().iter().enumerate() {
            buf[i] ^= *b;
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        extern "C" fn on_signal(_sig: i32) {
            STOP.store(true, Ordering::SeqCst);
        }
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
    }
    // Windows has no signals: the console handler fires on Ctrl-C / Ctrl-Break / log-off, and the
    // shell that started this process stops it by asking it to leave over the same HTTP route the
    // window's "Quit completely" uses (`POST /api/shutdown`). A process with no console at all
    // (detached) can still be stopped by that route, which is why the route, not the signal, is
    // what the shells rely on.
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
            STOP.store(true, Ordering::SeqCst);
            1
        }
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}
