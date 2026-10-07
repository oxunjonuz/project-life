//! One place where the interface server asks the platform a question.
//!
//! Round 301: this program stopped being a macOS-only child. Three shells start it now — the arm64
//! `Project Life.app` on macOS, a GTK/WebKitGTK shell on Linux and a Win32/WebView2 shell on
//! Windows — and the server itself is written once, in the files beside this one. Everything that
//! genuinely differs between those three platforms is collected here, so a route, a job or a log
//! line never has to ask which system it is on.
//!
//! Two rules, the same ones the rest of the program keeps:
//!
//! * **Nothing is guessed.** Where a platform cannot answer, the answer says so ("not available on
//!   this platform") instead of returning a value that looks like an answer.
//! * **Nothing pretends to have been run.** What is under `cfg(windows)` was compiled by
//!   `cargo build --target x86_64-pc-windows-gnu` and never executed on Windows, because there is no
//!   Windows machine here. `docs/LIMITATIONS.md` and `docs/PLATFORMS.md` say so, and name the one
//!   command that checks each of these calls on a machine that has Windows.

use std::path::PathBuf;

/// Can one process hand a listening socket to another one here?
///
/// macOS and Linux: yes, and `listen::acquire` uses it (the app binds, the child serves). Windows:
/// no — a socket *handle* can be duplicated into a child at CreateProcess time, but not handed over
/// a socket afterwards, so the server binds its own port on 127.0.0.1 through the ordinary ladder.
pub const DESCRIPTOR_PASSING: bool = cfg!(unix);

/// Where the app keeps its own folder: logs, `diagnose.txt`, the shell's handshake file.
///
/// The rule follows the platform's own convention rather than one shared path, because that is
/// where a person (and a crash report) looks:
///   * macOS  — `~/Library/Application Support/ProjectLife`
///   * Linux  — `$XDG_STATE_HOME/projectlife-app`, else `~/.local/state/projectlife-app`
///   * Windows— `%LOCALAPPDATA%\ProjectLife`, else `%APPDATA%\ProjectLife`
/// `PROJECTLIFE_APP_HOME` overrides all three (that is how the tests keep two shells apart).
pub fn app_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PROJECTLIFE_APP_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home).join("Library/Application Support/ProjectLife");
            }
        }
    }
    if cfg!(windows) {
        for key in ["LOCALAPPDATA", "APPDATA"] {
            if let Ok(v) = std::env::var(key) {
                if !v.is_empty() {
                    return PathBuf::from(v).join("ProjectLife");
                }
            }
        }
        return std::env::temp_dir().join("ProjectLife");
    }
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        if !state.is_empty() {
            return PathBuf::from(state).join("projectlife-app");
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home).join(".local/state/projectlife-app"),
        _ => std::env::temp_dir().join("projectlife-app"),
    }
}

/// This process's id.
pub fn pid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getpid() as u32 }
    }
    #[cfg(windows)]
    {
        win::current_pid()
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::process::id()
    }
}

/// The pid that started this process, or 0 when the platform will not say.
///
/// It is reported, never relied on: `--diagnose` prints it because "which process was refused"
/// is the question a sandbox answers with.
pub fn parent_pid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getppid() as u32 }
    }
    #[cfg(windows)]
    {
        win::parent_pid().unwrap_or(0)
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

/// Ask a process to stop, in the way that platform has.
///
/// Unix: `SIGTERM`, so the daemon stops between cycles and finishes the one it is in. Windows: there
/// is no signal to send to a detached process, and `TerminateProcess` would kill it mid-write, so
/// the daemon is asked the one way that works on every platform — the request file the core writes
/// (`projectlife daemon stop`, `archive::request_stop`), which the daemon reads on every cycle.
/// `terminate` is what is left for a process that ignored the request; it is never the first move.
pub fn terminate(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let r = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("kill({pid}, SIGTERM): {}", std::io::Error::last_os_error()))
        }
    }
    #[cfg(windows)]
    {
        win::terminate(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Err("this platform has no way to ask a process to stop".to_string())
    }
}


/// The daemon's own binary is spawned detached: no console window, and it keeps running after the
/// shell that started it has gone. On unix there is nothing to do — a child outlives its parent.
pub fn detach(_cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Fill `buf` with bytes the operating system considers unpredictable.
///
/// Returns false when it could not, so the caller can fall back to something weaker *and say so*,
/// rather than silently minting a token that is guessable.
pub fn random_bytes(buf: &mut [u8]) -> bool {
    #[cfg(unix)]
    {
        use std::io::Read;
        match std::fs::File::open("/dev/urandom") {
            Ok(mut f) => f.read_exact(buf).is_ok(),
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        win::random_bytes(buf)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = buf;
        false
    }
}

/// One line naming the operating system, in the system's own words.
pub fn os_line() -> String {
    #[cfg(unix)]
    {
        crate::diag::unix_os_line()
    }
    #[cfg(windows)]
    {
        win::os_line()
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }
}


// ---------------------------------------------------------------------------------------------
// Windows
//
// Written against the real function signatures (the same discipline as the macOS shell, which is
// compiled against Apple's headers so that the compiler — not my memory — checks every call).
// None of it has been executed: there is no Windows machine in this round.
// ---------------------------------------------------------------------------------------------
#[cfg(windows)]
pub mod win {
    pub type Handle = *mut core::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetCurrentProcessId() -> u32;
        pub fn GetCurrentProcess() -> Handle;
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        pub fn CloseHandle(h: Handle) -> i32;
        pub fn TerminateProcess(h: Handle, code: u32) -> i32;
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            h: Handle,
            class: u32,
            info: *mut core::ffi::c_void,
            len: u32,
            ret: *mut u32,
        ) -> i32;
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(alg: Handle, buf: *mut u8, len: u32, flags: u32) -> i32;
    }

    /// `PROCESS_TERMINATE | SYNCHRONIZE` — what the last-resort kill needs.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const TERMINATE: u32 = 0x0001;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcessBasicInformation {
        exit_status: i32,
        peb_base_address: *mut core::ffi::c_void,
        affinity_mask: usize,
        base_priority: i32,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
    }

    impl Default for ProcessBasicInformation {
        fn default() -> Self {
            ProcessBasicInformation {
                exit_status: 0,
                peb_base_address: core::ptr::null_mut(),
                affinity_mask: 0,
                base_priority: 0,
                unique_process_id: 0,
                inherited_from_unique_process_id: 0,
            }
        }
    }

    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }

    pub fn current_pid() -> u32 {
        unsafe { GetCurrentProcessId() }
    }

    /// The process that started this one, read with `NtQueryInformationProcess`. `Err` when the
    /// call is refused — the report then says so instead of inventing a number.
    pub fn parent_pid() -> Result<u32, String> {
        let mut info = ProcessBasicInformation::default();
        let mut ret: u32 = 0;
        let rc = unsafe {
            NtQueryInformationProcess(
                GetCurrentProcess(),
                0,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<ProcessBasicInformation>() as u32,
                &mut ret,
            )
        };
        if rc < 0 {
            return Err(format!("NtQueryInformationProcess returned 0x{:08x}", rc as u32));
        }
        Ok(info.inherited_from_unique_process_id as u32)
    }

    /// The last resort: kill it. Only ever called after the request file was ignored.
    pub fn terminate(pid: u32) -> Result<(), String> {
        let h = unsafe { OpenProcess(TERMINATE | SYNCHRONIZE, 0, pid) };
        if h.is_null() {
            return Err(format!("OpenProcess({pid}): {}", std::io::Error::last_os_error()));
        }
        let ok = unsafe { TerminateProcess(h, 0) };
        let err = if ok == 0 { Some(std::io::Error::last_os_error().to_string()) } else { None };
        unsafe { CloseHandle(h) };
        match err {
            None => Ok(()),
            Some(e) => Err(format!("TerminateProcess({pid}): {e}")),
        }
    }

    pub fn random_bytes(buf: &mut [u8]) -> bool {
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG: no algorithm handle to open, no library to initialise.
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
        let rc = unsafe {
            BCryptGenRandom(
                core::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        rc == 0
    }

    /// `RtlGetVersion` (not `GetVersionExW`): it reports the true version whatever the application
    /// manifest says, which is exactly what a diagnostic should print.
    pub fn os_line() -> String {
        let mut info = OsVersionInfo {
            size: std::mem::size_of::<OsVersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd: [0; 128],
        };
        let rc = unsafe { RtlGetVersion(&mut info) };
        if rc < 0 {
            return format!("Windows (version unknown: RtlGetVersion returned 0x{:08x})", rc as u32);
        }
        let csd: String = info
            .csd
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| char::from_u32(*c as u32).unwrap_or('?'))
            .collect();
        let arch = std::env::consts::ARCH;
        if csd.is_empty() {
            format!("Windows {}.{} (build {}) {}", info.major, info.minor, info.build, arch)
        } else {
            format!("Windows {}.{} (build {}) {} {csd}", info.major, info.minor, info.build, arch)
        }
    }


}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_folder_follows_the_platform_and_can_be_overridden() {
        let old = std::env::var("PROJECTLIFE_APP_HOME").ok();
        std::env::set_var("PROJECTLIFE_APP_HOME", "/tmp/pl-sys-test");
        assert_eq!(app_data_dir(), PathBuf::from("/tmp/pl-sys-test"));
        match old {
            Some(v) => std::env::set_var("PROJECTLIFE_APP_HOME", v),
            None => std::env::remove_var("PROJECTLIFE_APP_HOME"),
        }
    }

    #[test]
    fn random_bytes_are_not_all_zeroes_and_do_not_repeat() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        assert!(random_bytes(&mut a));
        assert!(random_bytes(&mut b));
        assert_ne!(a, b);
        assert!(a.iter().any(|x| *x != 0));
    }

    #[test]
    fn the_platform_line_names_something() {
        let l = os_line();
        assert!(!l.trim().is_empty());
    }
}
