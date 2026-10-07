//! Getting a socket to serve the window on — and saying exactly what went wrong when it cannot.
//!
//! Round 295 shipped this app with `--port 0` and one line of failure text. On the owner's Mac the
//! interface child was refused by the system:
//!
//! ```text
//! projectlife-ui: cannot listen on 127.0.0.1:0: Operation not permitted
//! ```
//!
//! The window then showed a generic sentence ("Reinstall the app") that named neither the cause nor
//! the process it came from. Two things were wrong with that, and both are fixed here:
//!
//! 1. **The reason is kept.** Every address that was tried is recorded with the errno the system
//!    returned, and those records travel to the window, to `daemon.log` and to `--diagnose`. A
//!    refusal and a busy port are told apart, because they need different answers from a person.
//! 2. **The port is chosen, not handed out blindly.** The kernel's choice (port 0) is now the *last*
//!    resort. The first attempt is an explicit local port, then a short ladder of neighbours, and
//!    only then an ephemeral one.
//!
//! And one more path exists, because the cause we could not reproduce pointed at a *process* being
//! denied rather than a port being busy: if this process cannot open a listening socket at all, it
//! asks the app that launched it for one. The app binds (a different process, a bundle the system
//! can attribute), and passes the descriptor over a unix socket with `SCM_RIGHTS`. The child then
//! serves on that socket. Nothing about this path is silent: which one was used is printed, logged,
//! and reported back to the window as `"how"`.

#[cfg(unix)]
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
// The descriptor type, and the unix socket, exist on unix only. Windows gets a type alias and a
// refusal with a sentence (see `acquire`), so the platform is answered in one place rather than
// pushing `cfg` into every call site.
#[cfg(unix)]
pub type ListenFd = std::os::unix::io::RawFd;
#[cfg(not(unix))]
pub type ListenFd = i32;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// The port the window's own server asks for first. High, unassigned, and unlikely to collide with
/// anything a developer runs by hand.
pub const DEFAULT_PORT: u16 = 7717;
/// How many neighbours to try before falling back to the kernel's choice.
pub const DEFAULT_RANGE: u16 = 10;
/// How long to wait for the app to hand over a socket before giving up and reporting.
#[cfg(unix)]
const HANDOFF_WAIT: Duration = Duration::from_secs(12);

/// One address we tried, and what the system said about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    pub addr: String,
    /// `ok`, `in_use`, `permission`, `unsupported`, or `error` — a stable word the window and the
    /// tests can branch on.
    pub kind: String,
    /// The system's own words, unchanged.
    pub detail: String,
}

impl Attempt {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({"addr": self.addr, "kind": self.kind, "detail": self.detail})
    }
}

/// Why no socket could be opened, in the terms the person in front of the screen needs.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub attempts: Vec<Attempt>,
    /// What happened when we asked the app for a socket, if we got that far.
    pub handoff: Option<String>,
    /// The most specific single line — the system's own words for the last attempt.
    pub last: String,
    pub kind: String,
}

impl Refusal {
    /// One sentence a person can act on. The distinction that matters: a busy port is this machine
    /// being busy; a permission refusal is this machine refusing *this process*, and no port number
    /// will change that.
    pub fn advice(&self) -> String {
        let denied = self.attempts.iter().filter(|a| a.kind == "permission").count();
        let in_use = self.attempts.iter().filter(|a| a.kind == "in_use").count();
        if denied > 0 && denied == self.attempts.len() {
            "The system refused the listening socket for this process — every local address was \
             answered with a permission error, so no port number will help. This is not a busy \
             port."
                .to_string()
        } else if in_use > 0 && in_use == self.attempts.len() {
            "Every port that was tried is already in use by something else on this machine."
                .to_string()
        } else if denied > 0 {
            format!(
                "Some addresses were refused by the system ({denied}) and others failed for other \
                 reasons ({}) — see the list.",
                self.attempts.len() - denied
            )
        } else {
            "No local address could be opened for listening.".to_string()
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!("could not open a listening socket: {}", self.last)];
        for a in &self.attempts {
            v.push(format!("  tried {:<22} {} ({})", a.addr, a.detail, a.kind));
        }
        if let Some(h) = &self.handoff {
            v.push(format!("  asking the app for a socket: {h}"));
        }
        v.push(format!("  {}", self.advice()));
        v
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "reason": self.last,
            "kind": self.kind,
            "advice": self.advice(),
            "attempts": self.attempts.iter().map(|a| a.json()).collect::<Vec<_>>(),
            "handoff": self.handoff,
        })
    }
}

/// A socket to serve on, and the provenance of it.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Acquired {
    pub listener: TcpListener,
    pub port: u16,
    /// `explicit`, `ladder`, `ephemeral`, `inherited`, `handed-over`.
    pub how: String,
    pub attempts: Vec<Attempt>,
    pub warnings: Vec<String>,
}

fn classify(e: &std::io::Error) -> (String, String) {
    let detail = e.to_string();
    let kind = match e.kind() {
        std::io::ErrorKind::PermissionDenied => "permission",
        std::io::ErrorKind::AddrInUse => "in_use",
        std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported => "unsupported",
        _ => "error",
    };
    // The errno is worth keeping even when the kind is a known one: `EACCES` and `EPERM` both land
    // in PermissionDenied, and on macOS they are told apart by people who know what they mean.
    let errno = e.raw_os_error().map(|n| format!("{n}")).unwrap_or_else(|| "?".into());
    (kind.to_string(), format!("{detail} (errno {errno})"))
}

/// The ports to try, in order. An explicit `--port 0` means the caller asked for the kernel's own
/// choice and gets exactly that — nothing else is added, so a caller can pin the behaviour.
pub fn candidates(port: u16, range: u16) -> Vec<u16> {
    if port == 0 {
        return vec![0];
    }
    let mut v = Vec::new();
    for i in 0..range.max(1) {
        let p = port.wrapping_add(i);
        if p != 0 {
            v.push(p);
        }
    }
    v.push(0);
    v
}

/// A fault injector for the tests and for `verify.sh`: make every local bind fail with the same
/// words the owner's Mac produced, so the refusal and handoff paths can be exercised anywhere.
/// Nothing in the app ever sets this; it is read once and reported wherever it takes effect.
fn injected_denial() -> Option<String> {
    std::env::var("PROJECTLIFE_UI_BIND_DENY").ok().map(|v| {
        let detail = if v.is_empty() { "1".to_string() } else { v };
        format!("Operation not permitted (errno 1) [injected by PROJECTLIFE_UI_BIND_DENY={detail}]")
    })
}

/// The real thing: bind 127.0.0.1:<port> in this process. The fault injector above turns every
/// attempt into the refusal the owner's Mac produced, so the paths below can be walked anywhere.
pub fn bind_local(port: u16) -> Result<TcpListener, Attempt> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if let Some(injected) = injected_denial() {
        return Err(Attempt { addr: addr.to_string(), kind: "permission".into(), detail: injected });
    }
    match TcpListener::bind(addr) {
        Ok(l) => Ok(l),
        Err(e) => {
            let (kind, detail) = classify(&e);
            Err(Attempt { addr: addr.to_string(), kind, detail })
        }
    }
}

/// The whole acquisition, in the order a person would try it.
///
/// `ipc` is the unix socket the app waits on when it has to bind on our behalf; `None` disables the
/// handoff (the browser-driven tests and the command line use that).
pub fn acquire(
    binder: &mut dyn FnMut(u16) -> Result<TcpListener, Attempt>,
    port: u16,
    range: u16,
    listen_fd: Option<ListenFd>,
    ipc: Option<&Path>,
    log: &mut dyn FnMut(&str),
    announce: &mut dyn FnMut(&Path, u16, &[Attempt]),
) -> Result<Acquired, Refusal> {
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // 0. A descriptor the app bound for us. Verified before use: a wrong guess here would mean
    //    serving on somebody else's socket, so the check is strict and its failure is spoken.
    //
    //    Windows: this platform does not hand a listening socket to a running process at all (a
    //    socket handle is duplicated into a child at CreateProcess time, which is a different
    //    mechanism and not this one). The parameter is therefore refused in words, recorded in the
    //    report the window can show, and the ladder above is the whole route there.
    #[cfg(not(unix))]
    if let Some(fd) = listen_fd {
        let line = format!(
            "the inherited descriptor {fd} was offered, but this platform hands no descriptor between \
             running processes — the port ladder is the route here"
        );
        log(&line);
        warnings.push(line);
        attempts.push(Attempt {
            addr: format!("inherited fd {fd}"),
            kind: "unsupported".into(),
            detail: "no descriptor passing on this platform".into(),
        });
    }
    #[cfg(unix)]
    if let Some(fd) = listen_fd {
        match verify_listener(fd, port) {
            Ok(p) => {
                log(&format!("using the socket the app opened (fd {fd}, 127.0.0.1:{p})"));
                let listener = unsafe { TcpListener::from_raw_fd(fd) };
                let _ = listener.set_nonblocking(true);
                return Ok(Acquired { listener, port: p, how: "inherited".into(), attempts, warnings });
            }
            Err(e) => {
                let line = format!("the inherited descriptor {fd} is not a usable local listener: {e}");
                log(&line);
                warnings.push(line);
                attempts.push(Attempt {
                    addr: format!("inherited fd {fd}"),
                    kind: "unsupported".into(),
                    detail: e,
                });
            }
        }
    }

    // 1. The ladder.
    let cands = candidates(port, range);
    for (i, p) in cands.iter().enumerate() {
        match binder(*p) {
            Ok(l) => {
                let real = l.local_addr().map(|a| a.port()).unwrap_or(*p);
                let how = if *p == 0 {
                    "ephemeral"
                } else if i == 0 {
                    "explicit"
                } else {
                    "ladder"
                };
                attempts.push(Attempt {
                    addr: format!("127.0.0.1:{p}"),
                    kind: "ok".into(),
                    detail: "listening".into(),
                });
                log(&format!("listening on 127.0.0.1:{real} ({how}; {} address(es) tried)", attempts.len()));
                let _ = l.set_nonblocking(true);
                return Ok(Acquired { listener: l, port: real, how: how.into(), attempts, warnings });
            }
            Err(a) => {
                log(&format!("127.0.0.1:{p} refused: {}", a.detail));
                attempts.push(a);
            }
        }
    }

    // 2. Ask the app that launched us. Only the app can answer: the path is a unix socket in the
    //    app's own support folder, and the descriptor arrives with SCM_RIGHTS.
    let mut handoff_note = None;
    #[cfg(not(unix))]
    if ipc.is_some() {
        let _ = &announce;
        let note = "not available on this platform: the handoff is a unix socket carrying a \
                    descriptor, so it exists on macOS and Linux only"
            .to_string();
        log(&note);
        handoff_note = Some(note);
    }
    #[cfg(unix)]
    if let Some(path) = ipc {
        announce(path, port, &attempts);
        match request_socket(path, port, log) {
            Ok((fd, from_port)) => {
                log(&format!("the app handed over its socket (127.0.0.1:{from_port})"));
                let listener = unsafe { TcpListener::from_raw_fd(fd) };
                let _ = listener.set_nonblocking(true);
                return Ok(Acquired { listener, port: from_port, how: "handed-over".into(), attempts, warnings });
            }
            Err(e) => {
                log(&format!("the app could not hand over a socket: {e}"));
                handoff_note = Some(e);
            }
        }
    }

    let last = attempts
        .last()
        .map(|a| format!("{} — {}", a.addr, a.detail))
        .unwrap_or_else(|| "no address was tried".to_string());
    let kind = attempts.last().map(|a| a.kind.clone()).unwrap_or_else(|| "error".into());
    Err(Refusal { attempts, handoff: handoff_note, last, kind })
}

/// Is this descriptor a TCP listener on the loopback address we expect? Asking the kernel rather
/// than trusting the caller: `getsockname` and two socket options.
#[cfg(unix)]
pub fn verify_listener(fd: ListenFd, want_port: u16) -> Result<u16, String> {
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let rc =
        unsafe { libc::getsockname(fd, &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr, &mut len) };
    if rc != 0 {
        return Err(format!("getsockname: {}", std::io::Error::last_os_error()));
    }
    if addr.sin_family as i32 != libc::AF_INET {
        return Err(format!("not an IPv4 socket (family {})", addr.sin_family));
    }
    let got_port = u16::from_be(addr.sin_port);
    let ip = u32::from_be(addr.sin_addr.s_addr);
    if ip != u32::from(Ipv4Addr::LOCALHOST) {
        return Err(format!("bound to {} — not the loopback address", std::net::Ipv4Addr::from(ip)));
    }
    if want_port != 0 && got_port != want_port {
        return Err(format!("bound to port {got_port}, not {want_port}"));
    }
    let mut ty: libc::c_int = 0;
    let mut tl = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_TYPE, &mut ty as *mut _ as *mut libc::c_void, &mut tl)
    } != 0
    {
        return Err(format!("SO_TYPE: {}", std::io::Error::last_os_error()));
    }
    if ty != libc::SOCK_STREAM {
        return Err(format!("socket type {ty} is not a stream"));
    }
    let mut acc: libc::c_int = 0;
    let mut al = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            &mut acc as *mut _ as *mut libc::c_void,
            &mut al,
        )
    } != 0
    {
        return Err(format!("SO_ACCEPTCONN: {}", std::io::Error::last_os_error()));
    }
    if acc == 0 {
        return Err("the socket is not listening".to_string());
    }
    Ok(got_port)
}

/// Bind a listening socket in this process. The tests that play the app's part on Linux use this;
/// on macOS the app itself binds (in Objective-C) and hands the descriptor over, so this is not on
/// the app's own path.
#[allow(dead_code)]
#[cfg(unix)]
pub fn bind_loopback(port: u16) -> Result<(ListenFd, u16, String), String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, 4);
    }
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr.s_addr = u32::from(Ipv4Addr::LOCALHOST).to_be();
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("bind(127.0.0.1:{port}): {e}"));
    }
    if unsafe { libc::listen(fd, 128) } != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("listen(127.0.0.1:{port}): {e}"));
    }
    match verify_listener(fd, 0) {
        Ok(p) => Ok((fd, p, "127.0.0.1".to_string())),
        Err(e) => {
            unsafe { libc::close(fd) };
            Err(e)
        }
    }
}

/// Ask whoever launched us for a socket. The request goes on the wire as one JSON line; the answer
/// is a descriptor in the ancillary data of the reply, or a refusal in its body.
/// Ask the app for a socket. The request goes on the wire as one JSON line; the answer is a
/// descriptor in the ancillary data of the reply, or a refusal in its body.
///
/// Note what this does **not** do: it never calls `bind`. That is the whole point. On the machine
/// where this went wrong, `bind` is the call the system refuses — so the app owns the unix socket
/// (it creates and listens on it before this process starts), and all this process does is connect
/// to it and receive. A route that needed the refused process to bind something else first would
/// have been no route at all.
#[cfg(unix)]
pub fn request_socket(path: &Path, port: u16, log: &mut dyn FnMut(&str)) -> Result<(ListenFd, u16), String> {
    log(&format!("asking the app for a socket on {}", path.display()));
    let deadline = Instant::now() + HANDOFF_WAIT;
    let mut s = loop {
        match UnixStream::connect(path) {
            Ok(s) => break s,
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(format!("cannot reach the app on {}: {e}", path.display()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let _ = s.set_read_timeout(Some(HANDOFF_WAIT));
    let req = serde_json::json!({"want": "socket", "port": port, "pid": std::process::id()});
    let mut line = serde_json::to_vec(&req).unwrap_or_default();
    line.push(b'\n');
    s.write_all(&line).map_err(|e| format!("cannot ask the app: {e}"))?;
    let _ = s.flush();

    let mut buf = [0u8; 4096];
    let (fd, n) = recv_fd(&s, &mut buf)?;
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let answer: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or(serde_json::Value::Null);
    match fd {
        Some(fd) => match verify_listener(fd, 0) {
            Ok(p) => Ok((fd, p)),
            Err(e) => {
                unsafe { libc::close(fd) };
                Err(format!("the app sent a descriptor that is not a local listener: {e}"))
            }
        },
        None => {
            let why = answer
                .get("reason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if text.trim().is_empty() {
                        "the app closed the connection without an answer".to_string()
                    } else {
                        text.trim().to_string()
                    }
                });
            Err(why)
        }
    }
}

/// The app's own side of the same protocol, so a test — or anything playing the app's part — can
/// speak it: listen on the unix socket, take one request, answer with a descriptor or a refusal.
#[allow(dead_code)]
#[cfg(unix)]
pub fn answer_one_request(listener: &UnixListener, fd: Option<ListenFd>, reason: &str) -> Result<String, String> {
    let (mut s, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 512];
    let n = s.read(&mut buf).map_err(|e| format!("read: {e}"))?;
    let asked = String::from_utf8_lossy(&buf[..n]).to_string();
    let body = match fd {
        Some(_) => b"{\"grant\":true}\n".to_vec(),
        None => format!("{{\"grant\":false,\"reason\":{}}}\n", serde_json::Value::from(reason)).into_bytes(),
    };
    send_fd(&s, &body, fd)?;
    Ok(asked)
}

/// Receive one message and, if it carries one, one descriptor.
#[cfg(unix)]
pub fn recv_fd(s: &UnixStream, buf: &mut [u8]) -> Result<(Option<ListenFd>, usize), String> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut libc::c_void, iov_len: buf.len() };
    let mut space = [0u8; 256];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1 as _;
    msg.msg_control = space.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = space.len() as _;
    let n = unsafe { libc::recvmsg(s.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(format!("recvmsg: {}", std::io::Error::last_os_error()));
    }
    let mut fd = None;
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            let c = &*cmsg;
            if c.cmsg_level == libc::SOL_SOCKET && c.cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg) as *const ListenFd;
                fd = Some(*data);
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok((fd, n.max(0) as usize))
}

/// Send one message and, if asked, one descriptor. The tests use this; on macOS the shell has its
/// own copy of it in Objective-C, and the two are compared by the same protocol, both directions.
#[allow(dead_code)]
#[cfg(unix)]
pub fn send_fd(s: &UnixStream, body: &[u8], fd: Option<ListenFd>) -> Result<(), String> {
    let mut iov = libc::iovec { iov_base: body.as_ptr() as *mut libc::c_void, iov_len: body.len() };
    let mut space = [0u8; 256];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1 as _;
    if let Some(fd) = fd {
        msg.msg_control = space.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::copy_nonoverlapping(&fd as *const ListenFd as *const u8, libc::CMSG_DATA(cmsg), 4);
        }
    }
    let n = unsafe { libc::sendmsg(s.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(format!("sendmsg: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Where the app and this process meet. Inside the app's own support folder, so it is per-user.
/// Where the app's side of the handoff listens. Only unix uses it; Windows is given the same
/// shape of path (it is never opened there) so that `--ipc` has one meaning everywhere.
pub fn ipc_path(app_home: &Path) -> PathBuf {
    app_home.join("ipc.sock")
}

/// Format a millisecond timestamp the way the log lines around it are formatted.
pub fn stamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60, (rem / 1000) % 60)
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn the_ladder_prefers_the_asked_for_port_and_ends_with_the_kernels_choice() {
        let c = candidates(7717, 3);
        assert_eq!(c, vec![7717, 7718, 7719, 0]);
    }

    #[test]
    fn an_explicit_zero_means_only_the_kernels_choice() {
        assert_eq!(candidates(0, 10), vec![0]);
    }

    #[test]
    fn a_busy_port_is_told_apart_from_a_refusal() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = held.local_addr().unwrap().port();
        let refused = bind_local(p).unwrap_err();
        assert_eq!(refused.kind, "in_use");
        let r = Refusal { attempts: vec![refused], handoff: None, last: "x".into(), kind: "in_use".into() };
        assert!(r.advice().contains("already in use"));
    }

    #[test]
    fn a_permission_refusal_says_no_port_number_will_help() {
        // The classification is driven from a real system error, not from a hand-written Attempt:
        // when this test wrote the kind itself it could not notice the mapping being changed, and a
        // mutant that called every refusal a busy port survived the whole campaign (M56).
        let err = std::io::Error::from_raw_os_error(libc::EPERM);
        let (kind, detail) = classify(&err);
        assert_eq!(kind, "permission", "{detail}");
        assert!(detail.contains("errno 1"), "{detail}");
        let a = Attempt { addr: "127.0.0.1:7717".into(), kind: kind.clone(), detail };
        let r = Refusal { attempts: vec![a], handoff: None, last: "x".into(), kind };
        assert!(r.advice().contains("no port number will help"));
        assert_eq!(r.kind, "permission");
        let j = r.json();
        assert_eq!(j["attempts"][0]["kind"], "permission");
    }

    #[test]
    fn the_ladder_moves_to_the_next_port_when_the_first_is_taken() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = held.local_addr().unwrap().port();
        let mut said = Vec::new();
        let got = acquire(&mut bind_local, p, 3, None, None, &mut |l: &str| said.push(l.to_string()), &mut |_, _, _| {})
            .unwrap();
        assert_ne!(got.port, p);
        assert_eq!(got.how, "ladder");
        assert_eq!(got.attempts[0].kind, "in_use");
        assert!(said.iter().any(|l| l.contains("refused")));
    }

    #[test]
    fn a_busy_first_port_does_not_reach_the_app_at_all() {
        // The ladder is the answer to a busy port: the app is never asked, and no announcement is
        // made, because the kernel's own choice is still available.
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = held.local_addr().unwrap().port();
        let mut announced = 0;
        let got = acquire(
            &mut bind_local,
            p,
            3,
            None,
            None,
            &mut |_| {},
            &mut |_, _, _| announced += 1,
        )
        .unwrap();
        assert_eq!(got.how, "ladder");
        assert_eq!(announced, 0);
    }

    #[test]
    fn every_address_denied_ends_in_a_refusal_that_names_the_denial() {
        let mut denied = |p: u16| -> Result<TcpListener, Attempt> {
            Err(Attempt {
                addr: format!("127.0.0.1:{p}"),
                kind: "permission".into(),
                detail: "Operation not permitted (errno 1)".into(),
            })
        };
        let r = acquire(&mut denied, 7717, 2, None, None, &mut |_| {}, &mut |_, _, _| {}).unwrap_err();
        assert_eq!(r.kind, "permission");
        assert_eq!(r.attempts.len(), 3, "two neighbours and the kernel's own choice");
        assert!(r.attempts.iter().all(|a| a.kind == "permission"));
        assert!(r.advice().contains("no port number will help"));
        assert!(r.handoff.is_none(), "with no ipc path there is nobody to ask");
    }

    #[test]
    fn every_address_denied_is_answered_by_the_app_handing_over_a_socket() {
        // The whole path in one test: the ladder is denied, the app is asked exactly once, it binds
        // in *its* process, passes the descriptor over, and the child serves on it.
        let dir = std::env::temp_dir().join(format!("pl-handoff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let ipc = dir.join("ipc.sock");
        let listener = app_listens(&ipc);
        let (fd, app_port, _) = bind_loopback(0).unwrap();
        let app = std::thread::spawn(move || {
            answer_one_request(&listener, Some(fd), "").unwrap();
            app_port
        });
        let mut denied = |p: u16| -> Result<TcpListener, Attempt> {
            Err(Attempt { addr: format!("127.0.0.1:{p}"), kind: "permission".into(), detail: "EPERM".into() })
        };
        let mut said = Vec::new();
        let mut announced = 0;
        let got = acquire(
            &mut denied,
            7717,
            2,
            None,
            Some(&ipc),
            &mut |l: &str| said.push(l.to_string()),
            &mut |_, _, a| {
                assert!(!a.is_empty());
                announced += 1;
            },
        )
        .unwrap();
        let app_port = app.join().unwrap();
        assert_eq!(got.port, app_port);
        assert_eq!(got.how, "handed-over");
        assert_eq!(announced, 1);
        assert!(said.iter().any(|l| l.contains("handed over")), "{said:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_handed_over_socket_is_verified_before_it_is_used() {
        // A plain file descriptor is not a listener: the child must say so rather than serve on it.
        if let Ok(f) = std::fs::File::open("/etc/hostname") {
            let bad = verify_listener(f.as_raw_fd(), 0);
            assert!(bad.is_err(), "a regular file must not pass for a listening socket");
        }
        let (fd, port, _) = bind_loopback(0).unwrap();
        assert_eq!(verify_listener(fd, 0).unwrap(), port);
        assert!(verify_listener(fd, port + 1).is_err(), "a mismatched port must be refused");
        unsafe { libc::close(fd) };
    }

    fn app_listens(path: &Path) -> UnixListener {
        let _ = std::fs::remove_file(path);
        UnixListener::bind(path).expect("the app's unix socket")
    }

    #[test]
    fn a_socket_can_travel_over_a_unix_socket() {
        let dir = std::env::temp_dir().join(format!("pl-fd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ipc.sock");
        let listener = app_listens(&path);
        let (fd, port, _) = bind_loopback(0).unwrap();
        let app = std::thread::spawn(move || answer_one_request(&listener, Some(fd), "").unwrap());
        let (got_fd, got_port) = request_socket(&path, 7717, &mut |_| {}).unwrap();
        let asked = app.join().unwrap();
        assert!(asked.contains("socket"), "{asked}");
        assert_eq!(got_port, port);
        assert_eq!(verify_listener(got_fd, port).unwrap(), port);
        unsafe { libc::close(got_fd) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_that_cannot_reach_the_app_says_so() {
        let dir = std::env::temp_dir().join(format!("pl-fd-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let e = request_socket(&dir.join("absent.sock"), 7717, &mut |_| {}).unwrap_err();
        assert!(e.contains("cannot reach the app"), "{e}");
        assert!(e.contains("absent.sock"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refusal_from_the_app_is_reported_not_invented() {
        let dir = std::env::temp_dir().join(format!("pl-fd-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ipc.sock");
        let listener = app_listens(&path);
        let app = std::thread::spawn(move || {
            answer_one_request(&listener, None, "the app could not bind either: Operation not permitted (errno 1)")
                .unwrap()
        });
        let e = request_socket(&path, 7717, &mut |_| {}).unwrap_err();
        let _ = app.join().unwrap();
        assert!(e.contains("the app could not bind either"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_descriptor_that_is_not_a_listener_never_reaches_the_server() {
        let dir = std::env::temp_dir().join(format!("pl-fd-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ipc.sock");
        let listener = app_listens(&path);
        let f = std::fs::File::open("/etc/hostname").unwrap();
        let fd = f.as_raw_fd();
        let app = std::thread::spawn(move || answer_one_request(&listener, Some(fd), "").unwrap());
        let e = request_socket(&path, 7717, &mut |_| {}).unwrap_err();
        let _ = app.join().unwrap();
        assert!(e.contains("not a local listener"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamps_are_iso_shaped() {
        let s = stamp(1_700_000_000_000);
        assert_eq!(s.len(), 19, "{s}");
        assert_eq!(&s[4..5], "-");
    }
}
