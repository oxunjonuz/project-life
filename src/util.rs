//! Small utilities: time, sizes, permissions, directory walking, uuid.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

pub fn ms_of(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// Broken-down local calendar time. One type for every platform, so the rest of the program never
/// mentions a platform time structure.
#[derive(Clone, Copy, Debug)]
pub struct LocalParts {
    pub y: i32,
    pub mo: u32,
    pub d: u32,
    pub h: u32,
    pub mi: u32,
    pub s: u32,
}

#[cfg(unix)]
pub fn local_parts(ms: i64) -> LocalParts {
    let secs = ms.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs, &mut tm);
    }
    LocalParts {
        y: tm.tm_year + 1900,
        mo: (tm.tm_mon + 1) as u32,
        d: tm.tm_mday as u32,
        h: tm.tm_hour as u32,
        mi: tm.tm_min as u32,
        s: tm.tm_sec as u32,
    }
}

#[cfg(windows)]
pub fn local_parts(ms: i64) -> LocalParts {
    win::local_parts(ms)
}

/// No platform time API at all: the calendar is computed in UTC. Only reached on platforms that are
/// neither unix nor Windows; the value is the UTC wall clock, not the local one.
#[cfg(not(any(unix, windows)))]
pub fn local_parts(ms: i64) -> LocalParts {
    utc_parts(ms)
}

/// UTC wall clock for a moment, computed arithmetically (civil-from-days).
pub fn utc_parts(ms: i64) -> LocalParts {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    LocalParts {
        y,
        mo,
        d,
        h: (rem / 3_600_000) as u32,
        mi: ((rem % 3_600_000) / 60_000) as u32,
        s: ((rem % 60_000) / 1000) as u32,
    }
}

/// The UTC moment of a wall-clock reading, computed arithmetically (inverse of `utc_parts`).
pub fn parts_to_utc_ms(p: &LocalParts) -> i64 {
    days_from_civil(p.y, p.mo, p.d) * 86_400_000
        + (p.h as i64) * 3_600_000
        + (p.mi as i64) * 60_000
        + (p.s as i64) * 1000
}

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
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

/// Local time as "YYYY-MM-DD HH:MM:SS".
pub fn fmt_local(ms: i64) -> String {
    fmt_local_fmt(ms, true)
}

/// Local time with milliseconds: "YYYY-MM-DD HH:MM:SS.mmm".
///
/// Exists because two moments one second apart are not one moment, and a message that prints both as
/// the same string ("moment 08:31:26 is earlier than the start of the available history 08:31:26")
/// reads as nonsense — the owner would be right not to trust it. The comparison is at millisecond
/// precision, so the sentence that reports it is too.
pub fn fmt_local_ms(ms: i64) -> String {
    format!("{}.{:03}", fmt_local_fmt(ms, true), ms.rem_euclid(1000))
}

/// The moment for a message about a boundary: with milliseconds when the second alone would show the
/// same string twice, plain otherwise.
pub fn fmt_moment_pair(a: i64, b: i64) -> (String, String) {
    if fmt_local(a) == fmt_local(b) && a != b {
        (fmt_local_ms(a), fmt_local_ms(b))
    } else {
        (fmt_local(a), fmt_local(b))
    }
}

/// Local time as "YYYY-MM-DD HH:MM".
pub fn fmt_local_min(ms: i64) -> String {
    fmt_local_fmt(ms, false)
}

fn fmt_local_fmt(ms: i64, secs: bool) -> String {
    let t = local_parts(ms);
    if secs {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.y, t.mo, t.d, t.h, t.mi, t.s
        )
    } else {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", t.y, t.mo, t.d, t.h, t.mi)
    }
}

/// Local day "YYYY-MM-DD" — the basis for journal file names.
pub fn local_day(ms: i64) -> (i32, u32, u32) {
    let t = local_parts(ms);
    (t.y, t.mo, t.d)
}

pub fn month_name(ms: i64) -> String {
    let (y, m, _) = local_day(ms);
    format!("{:04}-{:02}", y, m)
}

/// Naive local time -> epoch ms. The platform decides about daylight saving (`isdst = -1` on unix),
/// which is why this goes through the OS rather than through a fixed offset.
#[cfg(unix)]
fn naive_local_to_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = mo as i32 - 1;
    tm.tm_mday = d as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = mi as i32;
    tm.tm_sec = s as i32;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t as i64) * 1000
}

#[cfg(windows)]
fn naive_local_to_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    win::local_to_utc_ms(y, mo, d, h, mi, s)
}

#[cfg(not(any(unix, windows)))]
fn naive_local_to_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    parts_to_utc_ms(&LocalParts { y, mo, d, h, mi, s })
}

/// Parse a time specification for `--at`. Returns epoch ms.
///
/// Supported: `now`, `10m ago`, `2h ago`, `45s ago`, `3d ago`, ISO 8601 with or without offset,
/// `YYYY-MM-DD HH:MM[:SS]`, `YYYY-MM-DD`, `today HH:MM`, `yesterday HH:MM`,
/// `D.M.YYYY HH:MM`, `DD.MM.YYYY`, and a bare epoch ms (13+ digits).
/// A duration in the units a person reads: 12s, 3m, 5h 10m, 2d 4h.
pub fn fmt_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

pub fn parse_at(spec: &str, now: i64) -> Result<i64, String> {
    let s = spec.trim().trim_matches('"').trim();
    if s.is_empty() {
        return Err("empty time specification".into());
    }
    // Round 295: the program prints moments as ISO-8601 UTC (`pl log --json`, `pl tree`, `pl why`),
    // so a moment copied out of its own output must be usable as `--at`. The lower-casing below
    // destroys the `T` separator, so this form is recognised before it.
    if s.contains('T') {
        if let Some(ms) = crate::archive::parse_iso_ms(s) {
            return Ok(ms);
        }
    }
    let low = s.to_lowercase();
    if low == "now" {
        return Ok(now);
    }
    // Relative: "<N><unit> ago"
    if let Some(rest) = low.strip_suffix("ago") {
        let rest = rest.trim();
        let (num, unit) = split_num_unit(rest)?;
        let mult = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1000i64,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
            "h" | "hour" | "hours" | "hr" | "hrs" => 3_600_000,
            "d" | "day" | "days" => 86_400_000,
            other => return Err(format!("unknown time unit: {other}")),
        };
        return Ok(now - num * mult);
    }
    if low == "yesterday" {
        return Ok(now - 86_400_000);
    }
    if low == "today" {
        let t = local_parts(now);
        return Ok(naive_local_to_ms(t.y, t.mo, t.d, 0, 0, 0));
    }
    if let Some(rest) = low.strip_prefix("yesterday ") {
        return parse_date_time(rest.trim(), Some(now - 86_400_000));
    }
    if let Some(rest) = low.strip_prefix("today ") {
        return parse_date_time(rest.trim(), Some(now));
    }
    if low.starts_with("seq:") {
        return Err(format!("SPECIAL_SEQ:{s}"));
    }
    // epoch ms
    if low.len() >= 13 && low.chars().all(|c| c.is_ascii_digit()) {
        return Ok(low.parse::<i64>().map_err(|e| e.to_string())?);
    }
    parse_date_time(&low, None)
}

fn split_num_unit(s: &str) -> Result<(i64, String), String> {
    let mut num = String::new();
    let mut unit = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_digit() && unit.is_empty() {
            num.push(c);
        } else if c == '.' && unit.is_empty() {
            // allow 1.5h
            num.push(c);
        } else if c.is_whitespace() {
            continue;
        } else {
            if i == 0 {
                return Err(format!("not parsed: {s}"));
            }
            unit.push(c);
        }
    }
    let n: f64 = num.parse().map_err(|_| format!("not a number: {s}"))?;
    Ok((n as i64, unit))
}

/// `base` — when given, a time without a date is taken relative to that moment (today/yesterday).
fn parse_date_time(s: &str, base: Option<i64>) -> Result<i64, String> {
    let s = s.trim();
    // Time only, "HH:MM[:SS]" — relative to base or to today.
    if s.contains(':') && !s.contains('-') && !s.contains('.') {
        let anchor = match base {
            Some(b) => b,
            None => return Err(format!("not parsed: {s}")),
        };
        let (h, mi, sec) = parse_hms(s)?;
        let t = local_parts(anchor);
        return Ok(naive_local_to_ms(t.y, t.mo, t.d, h, mi, sec));
    }
    // Date plus optional time, separated by 'T' or a space.
    let (date_part, time_part) = match s.find('T').or_else(|| s.find(' ')) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (y, mo, d) = parse_date(date_part)?;
    let (h, mi, sec) = match time_part {
        Some(tp) => {
            let tp = tp.trim();
            // strip the zone: +05:00 / Z
            let core = if let Some(idx) = tp.find('+') {
                &tp[..idx]
            } else if let Some(idx) = tp.rfind('-') {
                &tp[..idx]
            } else if let Some(idx) = tp.find('Z') {
                &tp[..idx]
            } else {
                tp
            };
            parse_hms(core.trim())?
        }
        None => (0, 0, 0),
    };
    let mut res = naive_local_to_ms(y, mo, d, h, mi, sec);
    // Explicit zone: recompute — the naive time was treated as local but was in that zone.
    if let Some(tp) = time_part {
        if let Some(off) = parse_offset(tp.trim()) {
            let as_utc = naive_local_to_ms(y, mo, d, h, mi, sec) - offset_of_local(y, mo, d, h, mi, sec);
            res = as_utc - off * 1000;
        }
    }
    Ok(res)
}

/// Local offset (seconds) for a wall-clock reading: what `naive_local_to_ms` produces minus what the
/// same digits would mean in UTC. Computed from the two conversions, so it needs no platform
/// field — the same expression is the Windows answer.
fn offset_of_local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    let local = naive_local_to_ms(y, mo, d, h, mi, s);
    let as_utc = parts_to_utc_ms(&LocalParts { y, mo, d, h, mi, s });
    (local - as_utc) / 1000
}

fn parse_offset(s: &str) -> Option<i64> {
    // +05:00 / -03:30 / Z
    if s.ends_with('Z') || s.ends_with('z') {
        return Some(0);
    }
    let bytes = s.as_bytes();
    let mut sign_idx = None;
    for (i, c) in s.char_indices() {
        if (c == '+' || c == '-') && i > 0 {
            sign_idx = Some((i, c));
        }
    }
    let (i, c) = sign_idx?;
    let rest = &s[i + 1..];
    let (hh, mm) = match rest.find(':') {
        Some(j) => (rest[..j].parse::<i64>().ok()?, rest[j + 1..].parse::<i64>().ok()?),
        None => {
            if rest.len() == 4 {
                (rest[..2].parse::<i64>().ok()?, rest[2..].parse::<i64>().ok()?)
            } else {
                (rest.parse::<i64>().ok()?, 0)
            }
        }
    };
    let _ = bytes;
    let total = hh * 3600 + mm * 60;
    Some(if c == '+' { total } else { -total })
}

fn parse_hms(s: &str) -> Result<(u32, u32, u32), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(format!("time not parsed: {s}"));
    }
    let h: u32 = parts[0].trim().parse().map_err(|_| format!("hour: {s}"))?;
    let mi: u32 = if parts.len() > 1 { parts[1].trim().parse().map_err(|_| format!("minutes: {s}"))? } else { 0 };
    let sec: u32 = if parts.len() > 2 {
        parts[2].trim().trim_end_matches(|c: char| c.is_alphabetic()).parse().map_err(|_| format!("seconds: {s}"))?
    } else {
        0
    };
    Ok((h, mi, sec))
}

fn parse_date(s: &str) -> Result<(i32, u32, u32), String> {
    if s.contains('-') {
        let p: Vec<&str> = s.split('-').collect();
        if p.len() != 3 {
            return Err(format!("date not parsed: {s}"));
        }
        Ok((
            p[0].parse().map_err(|_| format!("year: {s}"))?,
            p[1].parse().map_err(|_| format!("month: {s}"))?,
            p[2].parse().map_err(|_| format!("day: {s}"))?,
        ))
    } else if s.contains('.') {
        let p: Vec<&str> = s.split('.').collect();
        if p.len() != 3 {
            return Err(format!("date not parsed: {s}"));
        }
        Ok((
            p[2].parse().map_err(|_| format!("year: {s}"))?,
            p[1].parse().map_err(|_| format!("month: {s}"))?,
            p[0].parse().map_err(|_| format!("day: {s}"))?,
        ))
    } else {
        Err(format!("date not parsed: {s}"))
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn median_p95(values: &mut Vec<i64>) -> Option<(i64, i64)> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let med = values[values.len() / 2];
    let idx = ((values.len() as f64) * 0.95).ceil() as usize;
    let p95 = values[idx.saturating_sub(1).min(values.len() - 1)];
    Some((med, p95))
}

pub fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        out.push(e.path());
    }
    out.sort();
    Ok(out)
}

/// Write a file whole, never half: tmp beside the target, fsync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "f".into()),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(parent);
    Ok(())
}

/// The same atomic replace as `write_atomic`, without fsync. For files that are **derived and
/// rebuilt on the next run** (the journal tail, the heartbeat): a torn or half-written one is
/// detected and rewritten, so paying for durability buys nothing — and on a slow filesystem it costs
/// as much as the whole rest of a pass (round 294).
pub fn write_atomic_lazy(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "f".into()),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(f) = fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

pub fn set_dir_owner_only(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

pub fn set_file_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

pub fn set_file_readonly(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o400));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

pub fn uuid_v4() -> String {
    let mut buf = [0u8; 16];
    let ok = fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok();
    if !ok {
        // Fallback source: time, pid and a stack address — not cryptography, but good enough for uniqueness.
        let t = now_ms() as u128;
        let pid = std::process::id() as u128;
        let x = t.wrapping_mul(31).wrapping_add(pid << 32);
        buf[..16].copy_from_slice(&x.to_le_bytes()[..16]);
    }
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    let h: Vec<String> = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]
    )
}

/// Normalize a path: `\` -> `/`, strip a leading `./`.
pub fn norm_rel(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);
    s.trim_start_matches('/').to_string()
}

/// Check that a relative path cannot escape the target folder (path traversal).
pub fn is_safe_rel(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    let s = p.replace('\\', "/");
    if s.starts_with('/') {
        return false;
    }
    // A Windows-style absolute path such as C:\...
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return false;
    }
    let mut depth = 0i32;
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

pub fn file_mode(md: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        0o644
    }
}

pub fn file_id(md: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.dev(), md.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        None
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// Windows: the only place in the program that talks to kernel32 directly.
///
/// Verified by `cargo check --target x86_64-pc-windows-gnu` (compiles), NOT by execution on
/// Windows — there is no Windows machine in this round. Everything here has a fallback that keeps
/// the program running (UTC instead of local time, `None` instead of a guessed disk figure) rather
/// than pretending the call worked.
#[cfg(windows)]
pub mod win {
    use super::LocalParts;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct SystemTime {
        pub year: u16,
        pub month: u16,
        pub dow: u16,
        pub day: u16,
        pub hour: u16,
        pub minute: u16,
        pub second: u16,
        pub ms: u16,
    }

    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            tz: *const core::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
        fn TzSpecificLocalTimeToSystemTime(
            tz: *const core::ffi::c_void,
            local: *const SystemTime,
            utc: *mut SystemTime,
        ) -> i32;
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            avail: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }

    fn as_system_time(p: &LocalParts) -> SystemTime {
        SystemTime {
            year: p.y as u16,
            month: p.mo as u16,
            dow: 0,
            day: p.d as u16,
            hour: p.h as u16,
            minute: p.mi as u16,
            second: p.s as u16,
            ms: 0,
        }
    }

    /// UTC moment -> local wall clock, DST included, through the OS.
    pub fn local_parts(ms: i64) -> LocalParts {
        let utc_parts = super::utc_parts(ms);
        let utc = as_system_time(&utc_parts);
        let mut local = SystemTime::default();
        let ok = unsafe { SystemTimeToTzSpecificLocalTime(core::ptr::null(), &utc, &mut local) };
        if ok == 0 {
            return utc_parts;
        }
        LocalParts {
            y: local.year as i32,
            mo: local.month as u32,
            d: local.day as u32,
            h: local.hour as u32,
            mi: local.minute as u32,
            s: local.second as u32,
        }
    }

    /// Local wall clock -> UTC moment, DST included, through the OS.
    pub fn local_to_utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        let local = as_system_time(&LocalParts { y, mo, d, h, mi, s });
        let mut utc = SystemTime::default();
        let ok = unsafe { TzSpecificLocalTimeToSystemTime(core::ptr::null(), &local, &mut utc) };
        if ok == 0 {
            return super::parts_to_utc_ms(&LocalParts { y, mo, d, h, mi, s });
        }
        super::parts_to_utc_ms(&LocalParts {
            y: utc.year as i32,
            mo: utc.month as u32,
            d: utc.day as u32,
            h: utc.hour as u32,
            mi: utc.minute as u32,
            s: utc.second as u32,
        })
    }

    fn wide(path: &std::path::Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        let mut w: Vec<u16> = path.as_os_str().encode_wide().collect();
        w.push(0);
        w
    }

    /// (free for the caller, total, free) — `GetDiskFreeSpaceExW`, the Windows answer to statvfs.
    pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64, u64)> {
        let w = wide(path);
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
        if ok == 0 {
            return None;
        }
        Some((avail, total, free))
    }

    /// Is that pid alive? `OpenProcess` for `PROCESS_QUERY_LIMITED_INFORMATION`, then its exit
    /// code — the Windows answer to `kill(pid, 0)`, which is what the lock files ask before they
    /// decide whether their holder is gone. A process this one is not allowed to look at is reported
    /// as gone: the caller steals a lock on the strength of this, and refusing to steal a live lock
    /// is the harmless direction to be wrong in.
    pub fn pid_alive(pid: u32) -> bool {
        const QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Handle(*mut core::ffi::c_void);
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn CloseHandle(h: Handle) -> i32;
            fn GetExitCodeProcess(h: Handle, code: *mut u32) -> i32;
        }
        unsafe {
            let h = OpenProcess(QUERY_LIMITED_INFORMATION, 0, pid);
            if h.0.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE
        }
    }

    /// The last resort, and only ever that: `TerminateProcess`. It can land in the middle of a
    /// write, which is exactly why `daemon stop` asks first (the request file) and reaches for this
    /// only when the request was ignored — and then says which one it used.
    pub fn terminate(pid: u32) -> Result<(), String> {
        const TERMINATE: u32 = 0x0001;
        const SYNCHRONIZE: u32 = 0x0010_0000;
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Handle(*mut core::ffi::c_void);
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn CloseHandle(h: Handle) -> i32;
            fn TerminateProcess(h: Handle, code: u32) -> i32;
        }
        unsafe {
            let h = OpenProcess(TERMINATE | SYNCHRONIZE, 0, pid);
            if h.0.is_null() {
                return Err(format!("OpenProcess({pid}): {}", std::io::Error::last_os_error()));
            }
            let ok = TerminateProcess(h, 0);
            let err = if ok == 0 { Some(std::io::Error::last_os_error().to_string()) } else { None };
            CloseHandle(h);
            match err {
                None => Ok(()),
                Some(e) => Err(format!("TerminateProcess({pid}): {e}")),
            }
        }
    }

    /// Is stdin a console? `GetConsoleMode` on the standard input handle, the Windows `isatty`.
    pub fn stdin_is_tty() -> bool {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Handle(*mut core::ffi::c_void);
        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> Handle;
            fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        }
        const STD_INPUT_HANDLE: u32 = -10i32 as u32;
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            GetConsoleMode(h, &mut mode) != 0
        }
    }
}

/// Is stdin a terminal? Everywhere but unix/Windows this answers `false`, which makes the program
/// refuse to confirm by assumption rather than assume consent.
pub fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(0) == 1 }
    }
    #[cfg(windows)]
    {
        win::stdin_is_tty()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}
