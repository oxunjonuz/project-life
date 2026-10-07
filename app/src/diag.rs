//! `--diagnose`: ask this machine, call by call, what it allows.
//!
//! When a socket is refused, the interesting question is *which step* was refused. "Cannot listen"
//! collapses four different system calls — `socket()`, `setsockopt()`, `bind()`, `listen()` — and a
//! sandbox, a firewall and a busy port all look the same from a single error line. This module runs
//! the steps separately and prints what each one answered, with the errno, plus what this process
//! is: its own path, its parent, the operating system, and the code signature the system sees.
//!
//! It never guesses. A probe that cannot be run says so; a probe that fails reports the system's own
//! words. The report is printed and also written to `diagnose.txt` next to `daemon.log`, so it can be
//! sent as one file.

#[cfg(unix)]
use crate::listen;
use std::io::Write;

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(unix)]
fn cstr_field(f: &[libc::c_char]) -> String {
    let p = f.as_ptr();
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().to_string()
}

/// The operating system, in the system's own words — whichever platform is asking.
pub fn os_line() -> String {
    #[cfg(unix)]
    {
        unix_os_line()
    }
    #[cfg(windows)]
    {
        crate::sys::win::os_line()
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }
}

#[cfg(unix)]
pub fn unix_os_line() -> String {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return format!("uname failed: {}", std::io::Error::last_os_error());
    }
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut s = format!(
        "{} {} ({})",
        cstr_field(&u.sysname),
        cstr_field(&u.release),
        cstr_field(&u.machine)
    );
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = Command::new("/usr/bin/sw_vers").output() {
            let t = String::from_utf8_lossy(&o.stdout).trim().replace('\n', " ");
            if !t.is_empty() {
                s.push_str(" — ");
                s.push_str(&t);
            }
        }
    }
    s
}

/// What the system believes this executable is. On macOS that includes the entitlements the code
/// signature carries, which is the single most useful fact when a socket is refused.
pub fn signature_line(exe: &Path) -> String {
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("/usr/bin/codesign")
            .arg("-d")
            .arg("--entitlements")
            .arg(":-")
            .arg("--verbose=4")
            .arg(exe)
            .output();
        match out {
            Ok(o) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                let t = text.trim();
                if t.is_empty() {
                    format!("codesign said nothing (exit {})", o.status.code().unwrap_or(-1))
                } else {
                    t.to_string()
                }
            }
            Err(e) => format!("cannot run /usr/bin/codesign: {e}"),
        }
    }
    #[cfg(windows)]
    {
        let _ = exe;
        "no Authenticode signature: these binaries are built unsigned (see docs/LIMITATIONS.md)"
            .to_string()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = exe;
        "not macOS: no code signature to read".to_string()
    }
}

/// What launched us. A per-process refusal and a per-machine one look different here.
pub fn parent_line() -> String {
    #[cfg(unix)]
    {
        unix_parent_line()
    }
    #[cfg(windows)]
    {
        windows_parent_line()
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!("pid {}", std::process::id())
    }
}

#[cfg(unix)]
fn unix_parent_line() -> String {
    let mine = unsafe { libc::getpid() };
    let parent = unsafe { libc::getppid() };
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = Command::new("/bin/ps").args(["-o", "command=", "-p", &parent.to_string()]).output() {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !t.is_empty() {
                return format!("pid {mine}, parent pid {parent}: {t}");
            }
        }
    }
    format!("pid {mine}, parent pid {parent}")
}

/// Windows: the pid is the system's own, and the parent is read with `NtQueryInformationProcess`.
/// When that call is refused the line says so — it does not print a zero that looks like an answer.
#[cfg(windows)]
fn windows_parent_line() -> String {
    let mine = crate::sys::pid();
    match crate::sys::win::parent_pid() {
        Ok(parent) => format!("pid {mine}, parent pid {parent}"),
        Err(e) => format!("pid {mine}, parent pid unknown ({e})"),
    }
}

#[cfg(unix)]
fn socket_probe(family: libc::c_int, ty: libc::c_int) -> Result<libc::c_int, String> {
    let fd = unsafe { libc::socket(family, ty, 0) };
    if fd < 0 {
        Err(format!("socket(family {family}): {}", std::io::Error::last_os_error()))
    } else {
        unsafe { libc::close(fd) };
        Ok(fd)
    }
}

/// Bind (and listen on) one address, exactly as the server would, and report what happened.
#[cfg(unix)]
pub fn bind_probe(addr: &str) -> Result<u16, String> {
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) => (h.trim_matches(|c| c == '[' || c == ']'), p.parse::<u16>().unwrap_or(0)),
        None => (addr, 0),
    };
    let v6 = host.contains(':') || host == "::1";
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!("socket: {}", std::io::Error::last_os_error()));
    }
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, 4);
    }
    let rc = if v6 {
        let mut a: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        a.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        a.sin6_port = port.to_be();
        a.sin6_addr.s6_addr = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        unsafe {
            libc::bind(
                fd,
                &a as *const libc::sockaddr_in6 as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    } else {
        let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        a.sin_family = libc::AF_INET as libc::sa_family_t;
        a.sin_port = port.to_be();
        a.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
        unsafe {
            libc::bind(
                fd,
                &a as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("bind: {} (errno {:?})", e, e.raw_os_error()));
    }
    if unsafe { libc::listen(fd, 16) } != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("listen: {}", e));
    }
    let got = listen::verify_listener(fd, 0);
    unsafe { libc::close(fd) };
    match got {
        Ok(p) => Ok(p),
        Err(e) => Err(format!("bound but not usable: {e}")),
    }
}

/// Windows: the same question — may this process listen on this address? — asked with the standard
/// library's socket calls, which are Winsock underneath. The vocabulary is the unix one on purpose
/// (`in use`, `permission`, `unsupported`), because the window branches on those words.
#[cfg(windows)]
pub fn bind_probe(addr: &str) -> Result<u16, String> {
    use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener};
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) => (h.trim_matches(|c| c == '[' || c == ']'), p.parse::<u16>().unwrap_or(0)),
        None => (addr, 0),
    };
    let sock = if host.contains(':') || host == "::1" {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
    } else {
        SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port))
    };
    match TcpListener::bind(sock) {
        Ok(l) => Ok(l.local_addr().map(|a| a.port()).unwrap_or(port)),
        Err(e) => Err(format!("bind: {e}")),
    }
}

/// A unix socket in the app's own folder. If TCP is refused but this works, the handoff path is
/// available — which is exactly what the app needs to know.
#[cfg(unix)]
pub fn uds_probe(home: &Path) -> Result<String, String> {
    let _ = std::fs::create_dir_all(home);
    let path = home.join("probe.sock");
    let _ = std::fs::remove_file(&path);
    match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => {
            let ok = l.local_addr().is_ok();
            drop(l);
            let _ = std::fs::remove_file(&path);
            if ok {
                Ok(path.display().to_string())
            } else {
                Err("a unix socket was created but has no address".to_string())
            }
        }
        Err(e) => Err(format!("bind: {e}")),
    }
}

/// The receive half of the handoff on its own: a descriptor is sent across a `socketpair` and
/// arrives. No `bind`, no path, nothing that a refusal could take away — this measures only whether
/// *this* process can receive a file descriptor at all, which is the part of the handoff the child
/// performs. It answers honestly in environments where the app's half cannot be opened here.
#[cfg(unix)]
pub fn pair_probe() -> (String, serde_json::Value) {
    use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } != 0 {
        return (
            format!("failed: socketpair: {}", std::io::Error::last_os_error()),
            serde_json::json!({"transport": "failed"}),
        );
    }
    let (a, b) = unsafe { (std::os::unix::net::UnixStream::from_raw_fd(fds[0]), std::os::unix::net::UnixStream::from_raw_fd(fds[1])) };
    let _ = a.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    if let Err(e) = crate::listen::send_fd(&a, b"{\"probe\":true}\n", Some(b.as_raw_fd())) {
        return (format!("failed: {e}"), serde_json::json!({"transport": "failed", "detail": e}));
    }
    let mut buf = [0u8; 128];
    match crate::listen::recv_fd(&b, &mut buf) {
        Ok((Some(fd), n)) => {
            let usable = unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1;
            unsafe { libc::close(fd) };
            if usable {
                (
                    format!("ok: a descriptor crossed a socketpair and is usable ({} byte(s) with it)", n),
                    serde_json::json!({"transport": "ok", "descriptor": "arrived"}),
                )
            } else {
                ("failed: a descriptor arrived but is not usable".to_string(),
                 serde_json::json!({"transport": "failed", "descriptor": "unusable"}))
            }
        }
        Ok((None, _)) => ("failed: the message arrived with no descriptor".to_string(),
                          serde_json::json!({"transport": "failed", "descriptor": "none"})),
        Err(e) => (format!("failed: {e}"), serde_json::json!({"transport": "failed", "detail": e})),
    }
}

/// The handoff transport, exercised end to end inside this one process: a unix socket is opened
/// here in the app's role, and here in the child's role it is asked for a descriptor. What this can
/// and cannot tell you is worth being exact about: it measures that the syscalls work (connect,
/// sendmsg with SCM_RIGHTS, recvmsg) in *this* process, not that the app's process is allowed more
/// than this one. The descriptor half is reported separately for that reason — under a refusal it
/// legitimately cannot be produced here, and that is not a failure of the transport.
#[cfg(unix)]
pub fn handoff_probe(home: &Path) -> (String, serde_json::Value) {
    let _ = std::fs::create_dir_all(home);
    let path = home.join("probe-ipc.sock");
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return (format!("failed: cannot open the app's side here: {e}"),
                           serde_json::json!({"transport": "failed", "detail": e.to_string()})),
    };
    let p2 = path.clone();
    let side = std::thread::spawn(move || {
        match crate::listen::bind_loopback(0) {
            Ok((fd, port, _)) => {
                let _ = crate::listen::answer_one_request(&listener, Some(fd), "");
                let _ = std::fs::remove_file(&p2);
                format!("bound a socket to hand over (port {port})")
            }
            Err(e) => {
                let _ = crate::listen::answer_one_request(&listener, None, &e);
                let _ = std::fs::remove_file(&p2);
                e
            }
        }
    });
    let r = crate::listen::request_socket(&path, 0, &mut |_| {});
    let app_said = side.join().unwrap_or_else(|_| "the app's side died".to_string());
    let _ = std::fs::remove_file(&path);
    match r {
        Ok((fd, port)) => {
            unsafe { libc::close(fd) };
            (
                format!("ok: asked, and a usable listener arrived (port {port}; {app_said})"),
                serde_json::json!({"transport": "ok", "descriptor": "arrived", "port": port}),
            )
        }
        Err(e) if e.contains("cannot reach the app") => (
            format!("failed: {e}"),
            serde_json::json!({"transport": "failed", "descriptor": "none", "detail": e}),
        ),
        Err(e) => (
            format!("half: the transport works (connect, request, answer) but no descriptor could be produced in this process ({e})"),
            serde_json::json!({"transport": "ok", "descriptor": "refused", "detail": e}),
        ),
    }
}

/// Windows has no unix socket in this program: the handoff it exists for does not exist here either,
/// so the honest report says that rather than running a probe whose answer would be meaningless.
#[cfg(windows)]
const NO_UNIX_SOCKET: &str = "not applicable on Windows: this app passes no descriptor between \
                              processes here (the server binds its own port)";

#[cfg(windows)]
pub fn uds_probe(_home: &Path) -> Result<String, String> {
    Err(NO_UNIX_SOCKET.to_string())
}

#[cfg(windows)]
pub fn pair_probe() -> (String, serde_json::Value) {
    (
        NO_UNIX_SOCKET.to_string(),
        serde_json::json!({"transport": "not_applicable", "descriptor": "not_applicable"}),
    )
}

#[cfg(windows)]
pub fn handoff_probe(_home: &Path) -> (String, serde_json::Value) {
    (
        NO_UNIX_SOCKET.to_string(),
        serde_json::json!({"transport": "not_applicable", "descriptor": "not_applicable"}),
    )
}

#[cfg(windows)]
fn socket_probe_name(name: &str) -> String {
    // What can be measured without libc: the two families this program actually uses. `unix` is
    // named as absent, which is the truth about this build rather than a failure.
    let (probe, _) = match name {
        "inet" => (bind_probe("127.0.0.1:0"), ()),
        "inet6" => (bind_probe("[::1]:0"), ()),
        "unix" => return "not applicable on Windows".to_string(),
        _ => (Err("unknown family".to_string()), ()),
    };
    match probe {
        Ok(_) => "created".to_string(),
        Err(e) => e,
    }
}
pub struct Report {
    pub lines: Vec<String>,
    pub json: serde_json::Value,
}

#[cfg(unix)]
pub fn report(home: &Path, port: u16) -> Report {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("projectlife-ui"));
    let mut lines = Vec::new();
    let mut j = serde_json::Map::new();

    lines.push(format!("projectlife-ui {}", env!("CARGO_PKG_VERSION")));
    lines.push(format!("executable: {}", exe.display()));
    lines.push(format!("os: {}", os_line()));
    lines.push(format!("process: {}", parent_line()));
    lines.push(format!("app folder: {}", home.display()));
    j.insert("os".into(), os_line().into());
    j.insert("process".into(), parent_line().into());
    j.insert("exe".into(), exe.display().to_string().into());

    let sig = signature_line(&exe);
    lines.push("code signature:".to_string());
    for l in sig.lines() {
        lines.push(format!("  {l}"));
    }
    j.insert("signature".into(), sig.into());

    lines.push("sockets:".to_string());
    let mut sockets = serde_json::Map::new();
    for (name, family, ty) in [
        ("inet", libc::AF_INET, libc::SOCK_STREAM),
        ("inet6", libc::AF_INET6, libc::SOCK_STREAM),
        ("unix", libc::AF_UNIX, libc::SOCK_STREAM),
    ] {
        let r = socket_probe(family, ty);
        let text = match &r {
            Ok(_) => "created".to_string(),
            Err(e) => e.clone(),
        };
        lines.push(format!("  socket({name}): {text}"));
        sockets.insert(name.into(), text.into());
    }
    j.insert("sockets".into(), sockets.into());

    lines.push("binding:".into());
    let mut binds = serde_json::Map::new();
    for addr in ["127.0.0.1:0", &format!("127.0.0.1:{port}"), &format!("[::1]:{port}")] {
        let r = bind_probe(addr);
        let text = match &r {
            Ok(p) => format!("bound, listening on port {p}"),
            Err(e) => e.clone(),
        };
        lines.push(format!("  {addr:<24} {text}"));
        binds.insert(addr.to_string(), text.into());
    }
    let uds = uds_probe(home);
    let uds_text = match &uds {
        Ok(p) => format!("bound ({p})"),
        Err(e) => e.clone(),
    };
    lines.push(format!("  {:<24} {uds_text}  (the app's side of the handoff)", "unix socket in the app folder"));
    binds.insert("unix socket in the app folder".into(), uds_text.into());
    j.insert("binds".into(), binds.into());

    let (handoff_text, handoff_json) = handoff_probe(home);
    lines.push("handoff (this process connects, the app's side is played here):".into());
    lines.push(format!("  {handoff_text}"));
    j.insert("handoff".into(), handoff_json.clone());

    // The sentence that matters, and it is derived, not assumed: written only from what the probes
    // above actually answered.
    let tcp_ok = j["binds"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.starts_with("127.0.0.1") || k.starts_with("[::1]"))
                .any(|(_, v)| v.as_str().unwrap_or("").starts_with("bound"))
        })
        .unwrap_or(false);
    let (pair_text, pair_json) = pair_probe();
    lines.push("descriptor transport (socketpair, no bind involved):".into());
    lines.push(format!("  {pair_text}"));
    j.insert("pair".into(), pair_json.clone());

    let transport_ok = handoff_json.get("transport").and_then(|v| v.as_str()) == Some("ok");
    let pair_ok = pair_json.get("transport").and_then(|v| v.as_str()) == Some("ok");
    let uds_ok = uds.is_ok();
    let verdict = if tcp_ok {
        "TCP listening works in this process; which port is used is decided by the ladder."
    } else if transport_ok {
        "TCP listening is refused in this process, but the refusal is about *binding*, not about \
         serving: this process can reach the app's socket, ask for one and receive the answer \
         (see the handoff probe above). A process that is allowed to bind — the app — can hand the \
         socket over, and this process then serves on it."
    } else if pair_ok && !uds_ok {
        "TCP listening is refused here, and this process cannot open a unix socket either, so the \
         handoff could not be measured end to end from here. The receive half itself works: a \
         descriptor crossed a socketpair. Whether the *app* may bind is not measurable from inside \
         this process — its own lines in daemon.log are that measurement."
    } else {
        "TCP listening is refused in this process and the handoff could not be reached either: see \
         the handoff line above for the system's own words."
    };
    lines.push(format!("verdict: {verdict}"));
    j.insert("verdict".into(), verdict.into());

    Report { lines, json: serde_json::Value::Object(j) }
}

/// The same report on Windows, built from the probes that exist there. Every key the unix report has
/// is present, so a reader (the window, a person, a bug report) does not have to know which platform
/// wrote it; the ones that have no meaning here say `not applicable` in words.
#[cfg(windows)]
pub fn report(home: &Path, port: u16) -> Report {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("projectlife-ui.exe"));
    let mut lines = Vec::new();
    let mut j = serde_json::Map::new();

    lines.push(format!("projectlife-ui {}", env!("CARGO_PKG_VERSION")));
    lines.push(format!("executable: {}", exe.display()));
    lines.push(format!("os: {}", os_line()));
    lines.push(format!("process: {}", parent_line()));
    lines.push(format!("app folder: {}", home.display()));
    j.insert("os".into(), os_line().into());
    j.insert("process".into(), parent_line().into());
    j.insert("exe".into(), exe.display().to_string().into());

    let sig = signature_line(&exe);
    lines.push("code signature:".to_string());
    for l in sig.lines() {
        lines.push(format!("  {l}"));
    }
    j.insert("signature".into(), sig.into());

    lines.push("sockets:".to_string());
    let mut sockets = serde_json::Map::new();
    for name in ["inet", "inet6", "unix"] {
        let text = socket_probe_name(name);
        lines.push(format!("  socket({name}): {text}"));
        sockets.insert(name.into(), text.into());
    }
    j.insert("sockets".into(), sockets.into());

    lines.push("binding:".to_string());
    let mut binds = serde_json::Map::new();
    let mut tcp_ok = false;
    for addr in ["127.0.0.1:0", &format!("127.0.0.1:{port}"), &format!("[::1]:{port}")] {
        let r = bind_probe(addr);
        let text = match &r {
            Ok(p) => {
                tcp_ok = true;
                format!("bound, listening on port {p}")
            }
            Err(e) => e.clone(),
        };
        lines.push(format!("  {addr:<24} {text}"));
        binds.insert(addr.to_string(), text.into());
    }
    let uds_text = uds_probe(home).unwrap_or_else(|e| e);
    binds.insert("unix socket in the app folder".into(), uds_text.clone().into());
    lines.push(format!("  {:<24} {uds_text}", "unix socket in the app folder"));
    j.insert("binds".into(), binds.into());

    let (handoff_text, handoff_json) = handoff_probe(home);
    lines.push("handoff:".to_string());
    lines.push(format!("  {handoff_text}"));
    j.insert("handoff".into(), handoff_json.clone());

    let (pair_text, pair_json) = pair_probe();
    lines.push("descriptor transport:".to_string());
    lines.push(format!("  {pair_text}"));
    j.insert("pair".into(), pair_json.clone());

    let verdict = if tcp_ok {
        "TCP listening works in this process; which port is used is decided by the ladder."
    } else {
        "TCP listening was refused in this process. On Windows there is no second route (no \
         descriptor handoff), so the reason above is the whole answer — most often another program \
         holds the port, or a security product refuses this program the right to listen."
    };
    lines.push(format!("verdict: {verdict}"));
    j.insert("verdict".into(), verdict.into());

    Report { lines, json: serde_json::Value::Object(j) }
}

/// Print the report, write it beside the log, and return the JSON line for stdout.
pub fn run(home: &Path, port: u16) -> serde_json::Value {
    let r = report(home, port);
    let text = r.lines.join("\n");
    println!("{text}");
    let _ = std::fs::create_dir_all(home);
    let path = home.join("diagnose.txt");
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = writeln!(f, "{text}");
        let _ = writeln!(f, "\n{}", serde_json::to_string_pretty(&r.json).unwrap_or_default());
    }
    serde_json::json!({"diagnose": r.json, "file": path.display().to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_report_covers_every_layer_a_refusal_could_come_from() {
        let dir = std::env::temp_dir().join(format!("pl-diag-{}", std::process::id()));
        let r = report(&dir, 7717);
        let j = &r.json;
        for key in ["os", "process", "exe", "signature", "sockets", "binds", "handoff", "pair", "verdict"] {
            assert!(j.get(key).is_some(), "the report has no {key}");
        }
        assert!(j["sockets"]["inet"].as_str().unwrap().contains("created"));
        assert!(j["binds"]["127.0.0.1:0"].as_str().unwrap().starts_with("bound"));
        assert!(j["binds"]["unix socket in the app folder"].as_str().unwrap().starts_with("bound"));
        assert_eq!(j["handoff"]["transport"].as_str().unwrap(), "ok");
        assert_eq!(j["handoff"]["descriptor"].as_str().unwrap(), "arrived");
        assert_eq!(j["pair"]["transport"].as_str().unwrap(), "ok");
        assert!(j["verdict"].as_str().unwrap().contains("TCP listening works"));
        assert!(r.lines.iter().any(|l| l.contains("verdict:")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("pl-diag-clean-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = uds_probe(&dir);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "the probe left {} file(s)", left.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_handoff_probe_reports_the_transport_and_the_descriptor_separately() {
        let dir = std::env::temp_dir().join(format!("pl-diag-handoff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (text, j) = handoff_probe(&dir);
        // Where bind works, both halves work; the wording must keep them apart either way.
        assert_eq!(j["transport"].as_str().unwrap(), "ok", "{text}");
        assert_eq!(j["descriptor"].as_str().unwrap(), "arrived", "{text}");
        assert!(text.starts_with("ok:"), "{text}");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "the probe left {} file(s) behind", left.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_descriptor_can_cross_a_socketpair_even_where_binding_is_impossible() {
        // No bind anywhere in this probe: it must work on a machine that refuses every bind, which
        // is exactly the machine this round was written for.
        let (text, j) = pair_probe();
        assert_eq!(j["transport"].as_str().unwrap(), "ok", "{text}");
        assert!(text.starts_with("ok:"), "{text}");
    }

    #[test]
    fn a_busy_port_is_reported_as_a_busy_port() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        let e = bind_probe(&format!("127.0.0.1:{p}")).unwrap_err();
        assert!(e.contains("Address already in use") || e.contains("in use"), "{e}");
    }

    #[test]
    fn the_report_is_written_where_it_says_it_is() {
        let dir = std::env::temp_dir().join(format!("pl-diag-write-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let j = run(&dir, 7717);
        let file = j["file"].as_str().unwrap();
        let text = std::fs::read_to_string(file).unwrap();
        assert!(text.contains("verdict:"), "the written report has no verdict");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
