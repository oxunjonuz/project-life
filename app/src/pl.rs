//! Talking to the core.
//!
//! The desktop app does not re-implement anything the core already does: every mutation (adding a
//! folder, restoring, exporting, importing, one observation pass) is a call to the `projectlife`
//! binary that ships inside the same bundle, and every read is either the same binary's `--json`
//! output or a file the core documents (`location.json`, `watch_state.json`, `heartbeat`).
//!
//! One deliberate exception: verifying a restore or an export. That check hashes the files that
//! came out and compares them with the hashes the archive recorded — done here, by a different
//! implementation than the one that wrote them, which is the only kind of check worth having.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone)]
pub struct Core {
    pub bin: PathBuf,
    pub archive: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Core {
    /// Build a command for the core binary, with the archive pinned when it is known.
    pub fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.bin);
        if let Some(a) = &self.archive {
            c.arg("--archive").arg(a);
        }
        if let Some(h) = &self.home {
            c.env("PROJECTLIFE_HOME", h);
        }
        c.env("NO_COLOR", "1");
        c.args(args);
        c
    }

    pub fn run(&self, args: &[String]) -> (i32, String, String) {
        match self.command(args).stdin(Stdio::null()).output() {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            ),
            Err(e) => (-1, String::new(), format!("cannot run {}: {e}", self.bin.display())),
        }
    }

    pub fn json(&self, args: &[String]) -> Result<serde_json::Value, String> {
        let (code, out, err) = self.run(args);
        if code != 0 {
            return Err(format!("{} exited {code}: {}", args.join(" "), first_lines(&err, 4)));
        }
        last_json(&out).ok_or_else(|| format!("no JSON in the output of `{}`", args.join(" ")))
    }

    pub fn version(&self) -> String {
        let (_c, out, _e) = self.run(&["version".to_string()]);
        out.lines().next().unwrap_or("").trim().to_string()
    }
}

pub fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join(" / ")
}

/// The last complete JSON value in a command's output.
///
/// The core prints human text and, with `--json`, the document last; some commands also print the
/// detection report first. Taking the *last* complete document is what makes the mix readable
/// without a second output format existing. The document's end is found by matching braces (and
/// ignoring braces inside strings), so text printed after the JSON — should a command ever do that —
/// does not hide it.
pub fn last_json(out: &str) -> Option<serde_json::Value> {
    let mut starts: Vec<usize> = vec![0];
    for (i, c) in out.char_indices() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    for &line_start in starts.iter().rev() {
        let rest = &out[line_start..];
        let trimmed = rest.trim_start();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            continue;
        }
        let start = line_start + (rest.len() - trimmed.len());
        // A value that begins right after a colon, comma or opening bracket is a *fragment* of a
        // document, not a document: the pretty-printed output of the core is full of those.
        if let Some(prev) = out[..start].chars().rev().find(|c| !c.is_whitespace()) {
            if matches!(prev, ':' | ',' | '{' | '[') {
                continue;
            }
        }
        if let Some(end) = balanced_end(out, start) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out[start..end]) {
                return Some(v);
            }
        }
    }
    None
}

/// Where the value that starts at `start` ends, counting braces outside strings.
fn balanced_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Where the core keeps the archive location. The same rules as `archive::location_file()`; the app
/// only reads this file, it never writes it (`pl init-archive` does that).
pub fn location_file(home: Option<&Path>) -> PathBuf {
    if let Some(h) = home {
        return h.join("projectlife").join("location.json");
    }
    if let Ok(h) = std::env::var("PROJECTLIFE_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h).join("projectlife").join("location.json");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("projectlife").join("location.json");
        }
    }
    // Kept identical to the core's `archive::location_file()`: two implementations of one rule are
    // two chances to disagree, and the archive on disk is the thing that must not move.
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_default();
        return PathBuf::from(home).join("Library/Application Support").join("projectlife/location.json");
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("projectlife/location.json");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("projectlife/location.json")
}

pub fn read_location(home: Option<&Path>) -> Option<PathBuf> {
    let text = fs::read_to_string(location_file(home)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("archiveRoot").and_then(|x| x.as_str()).map(PathBuf::from)
}

pub fn is_archive(root: &Path) -> bool {
    root.join("config.json").is_file() && root.join("projects").is_dir()
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// The size and sha256 of a file, remembered between calls.
///
/// The window asks "which build is this?" on every status poll, and re-reading three megabytes of
/// binary every three seconds would cost more than the answer is worth. The cache is keyed by the
/// file's own size and modification time, so a binary that is replaced under a running app is
/// re-hashed instead of being reported from memory — which is the whole point of asking.
pub fn file_hash_cached(path: &Path) -> Result<(u64, String), String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (u64, u64, String)>>> = OnceLock::new();
    let md = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = md.len();
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((s, m, h)) = g.get(path) {
            if *s == size && *m == mtime {
                return Ok((size, h.clone()));
            }
        }
    }
    let h = sha256_file(path)?;
    if let Ok(mut g) = cache.lock() {
        g.insert(path.to_path_buf(), (size, mtime, h.clone()));
    }
    Ok((size, h))
}

pub fn free_bytes(path: &Path) -> Option<u64> {
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
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

pub fn total_bytes(path: &Path) -> Option<u64> {
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
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Are these two paths on the same device? Used for the "keep the archive on another disk" advice.
pub fn same_device(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (d, t) = s.split_once('T')?;
    let mut dp = d.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    let (tm, frac) = match t.split_once('.') {
        Some((a, b)) => (a, b),
        None => (t, "0"),
    };
    let mut tp = tm.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next().unwrap_or("0").parse().ok()?;
    let ms: i64 = format!("{:0<3}", frac).chars().take(3).collect::<String>().parse().ok()?;
    let days = days_from_civil(y as i32, mo as u32, day as u32);
    Some(days * 86_400_000 + h * 3_600_000 + mi * 60_000 + sec * 1000 + ms)
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

/// Local time, from the system's own offset for that instant (no dependency on a timezone file).
pub fn fmt_local(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let offset = local_offset_seconds(secs);
    let local = ms + offset as i64 * 1000;
    let days = local.div_euclid(86_400_000);
    let rem = local.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3_600_000,
        (rem % 3_600_000) / 60_000,
        (rem % 60_000) / 1000
    )
}

#[cfg(unix)]
fn local_offset_seconds(unix_secs: i64) -> i64 {
    unsafe {
        let t = unix_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn local_offset_seconds(_unix_secs: i64) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_json_document_is_found_after_human_text() {
        let out = "Scanning /tmp/x...\n\nDetected profile: developer\n\n{\n  \"files\": 3,\n  \"bytes\": 47\n}\n";
        let v = last_json(out).expect("a document after the report");
        assert_eq!(v["files"], 3);
        assert_eq!(v["bytes"], 47);

        let out2 = "{\"fresh\": true}\nsome trailing line\n";
        assert_eq!(last_json(out2).unwrap()["fresh"], true);

        let out3 = "no json here at all\n";
        assert!(last_json(out3).is_none());

        let out4 = "{\"a\": 1}\n{\"b\": 2}\n";
        assert_eq!(last_json(out4).unwrap()["b"], 2, "the last document wins");

        // The pretty-printed output of `pl detect --json` is full of nested objects; a fragment must
        // never be mistaken for the document. This is a real regression: it happened once.
        let nested = "{\n  \"best\": {\n    \"profile\": \"developer\",\n    \"confidence\": 70\n  },\n  \"extensionCounts\": {\n    \".rs\": 1\n  }\n}\n";
        let v = last_json(nested).expect("the whole document, not a fragment");
        assert_eq!(v["best"]["profile"], "developer", "a nested object was returned instead of the document");
        assert_eq!(v["extensionCounts"][".rs"], 1);
    }

    #[test]
    fn hashing_a_file_matches_a_known_value() {
        let dir = std::env::temp_dir().join(format!("pl-app-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_iso_form_the_core_prints_parses() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_iso_utc("1970-01-02T00:00:00.000Z"), Some(86_400_000));
        // A moment from this project's own journal.
        assert_eq!(parse_iso_utc("2026-10-06T05:46:13.488Z"), Some(1_791_265_573_488));
        assert_eq!(parse_iso_utc("not a time"), None);
    }

    #[test]
    fn the_location_file_follows_the_same_rules_as_the_core() {
        let p = location_file(Some(Path::new("/tmp/home")));
        assert!(p.ends_with("projectlife/location.json"), "{}", p.display());
        assert!(p.starts_with("/tmp/home"));
    }

    #[test]
    fn an_archive_is_recognised_by_its_own_files() {
        let dir = std::env::temp_dir().join(format!("pl-app-arch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("projects")).unwrap();
        assert!(!is_archive(&dir));
        std::fs::write(dir.join("config.json"), b"{}").unwrap();
        assert!(is_archive(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn free_space_is_reported_for_a_real_folder() {
        assert!(free_bytes(Path::new("/tmp")).unwrap_or(0) > 0);
    }
}
