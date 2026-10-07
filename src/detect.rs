//! Detection: what kind of folder is this? Metadata only — names, sizes and extensions. No file
//! content is ever read here, and that is a promise the acceptance suite measures, not a hope.
//!
//! Scoring follows SPEC §6.17:
//!
//! ```text
//! score = 0.55 * extensionScore + 0.30 * markerScore + 0.10 * folderScore + 0.05 * sizeScore
//! ```
//!
//! with the components defined as:
//!   * extensionScore — of the folders' *informative* files (those whose extension some profile
//!     claims at all), the share this profile claims;
//!   * markerScore    — 1 when at least one of the profile's markers is present, else 0;
//!   * folderScore    — 1 when a directory name matches one of the profile's hints, else 0;
//!   * sizeScore      — of the files this profile claims, the share inside its size limit.
//!
//! A component the profile does not declare (a profile with no markers has no marker evidence to
//! give) is dropped and the remaining weights are renormalised, so "no markers declared" can never
//! be mistaken for "markers found". Anti-markers multiply the final score down by 0.15 each, at
//! most 0.30.

use crate::profiles::{self, Profile};
use crate::util;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const DEFAULT_MAX_ENTRIES: usize = 100_000;
pub const DEFAULT_MAX_MS: u64 = 5_000;
/// Files above this are reported as large: they are skipped unless the user says otherwise.
pub const LARGE_FILE_BYTES: u64 = 1_000_000_000;
pub const LOW_CONFIDENCE: u32 = 40;
pub const AMBIGUITY_GAP: u32 = 10;

const W_EXT: f64 = 0.55;
const W_MARKER: f64 = 0.30;
const W_FOLDER: f64 = 0.10;
const W_SIZE: f64 = 0.05;

#[derive(Clone, Debug, Default)]
pub struct Score {
    pub id: String,
    pub display_name: String,
    pub confidence: u32,
    pub extension_score: f64,
    pub marker_score: f64,
    pub folder_score: f64,
    pub size_score: f64,
    pub components: Vec<&'static str>,
    pub markers_hit: Vec<String>,
    pub folder_hits: Vec<String>,
    pub anti_markers_hit: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Detection {
    pub root: String,
    pub entries: usize,
    pub directories: usize,
    pub files: usize,
    /// Files counted but never stat'ed: their extension is one no profile claims and they are not
    /// media or archive attachments, so their size could not change any answer.
    pub unsized_files: usize,
    pub symlinks: usize,
    pub special: usize,
    pub truncated: bool,
    pub elapsed_ms: i64,
    pub extension_counts: BTreeMap<String, usize>,
    pub unknown_extensions: Vec<(String, usize)>,
    pub markers: Vec<String>,
    pub folder_hints: Vec<String>,
    pub safety_excluded: Vec<(String, String, String)>,
    pub skipped_dirs: Vec<(String, String)>,
    pub large_files: Vec<(String, u64)>,
    pub candidates: Vec<Score>,
    pub ambiguous: bool,
    pub suggest_custom: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub left_out_media: Vec<String>,
    pub max_file_size_kb: u64,
    pub estimated_files: usize,
    pub estimated_archive_bytes: u64,
    pub warnings: Vec<String>,
}

impl Detection {
    pub fn best(&self) -> Option<&Score> {
        self.candidates.first()
    }

    pub fn best_id(&self) -> Option<String> {
        self.candidates.first().map(|c| c.id.clone())
    }

    pub fn confidence(&self) -> u32 {
        self.candidates.first().map(|c| c.confidence).unwrap_or(0)
    }

    /// The profile the user would be offered: `custom` while nothing is convincing.
    pub fn suggested(&self) -> &str {
        match self.candidates.first() {
            Some(c) if c.confidence >= LOW_CONFIDENCE => c.id.as_str(),
            _ => "custom",
        }
    }

    pub fn runner_up_gap(&self) -> u32 {
        match (self.candidates.first(), self.candidates.get(1)) {
            (Some(a), Some(b)) => a.confidence.saturating_sub(b.confidence),
            _ => 100,
        }
    }

    pub fn to_json(&self) -> Value {
        let best = self.candidates.first();
        json!({
            "path": self.root,
            "entriesScanned": self.entries,
            "files": self.files,
            "unsizedFiles": self.unsized_files,
            "directories": self.directories,
            "symlinks": self.symlinks,
            "specialFiles": self.special,
            "truncated": self.truncated,
            "elapsedMs": self.elapsed_ms,
            "detectedProfile": best.map(|c| c.id.clone()),
            "displayName": best.map(|c| c.display_name.clone()),
            "confidence": self.confidence(),
            "ambiguous": self.ambiguous,
            "suggestCustom": self.suggest_custom,
            "extensionCounts": self.extension_counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>(),
            "unknownExtensions": self.unknown_extensions.iter().map(|(e, n)| json!({"extension": e, "files": n})).collect::<Vec<_>>(),
            "markers": self.markers,
            "folderHints": self.folder_hints,
            "candidates": self.candidates.iter().map(|c| json!({
                "profile": c.id,
                "displayName": c.display_name,
                "confidence": c.confidence,
                "extensionScore": round3(c.extension_score),
                "markerScore": round3(c.marker_score),
                "folderScore": round3(c.folder_score),
                "sizeScore": round3(c.size_score),
                "components": c.components,
                "markersHit": c.markers_hit,
                "folderHits": c.folder_hits,
                "antiMarkersHit": c.anti_markers_hit,
            })).collect::<Vec<_>>(),
            "suggestedInclude": self.include_globs,
            "suggestedExclude": self.exclude_globs,
            "notAutoIncluded": self.left_out_media,
            "maxFileSizeKb": self.max_file_size_kb,
            "estimatedFiles": self.estimated_files,
            "estimatedArchiveBytes": self.estimated_archive_bytes,
            "largeFiles": self.large_files.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
            "safetyExcluded": self.safety_excluded.iter().map(|(p, r, rule)| json!({"path": p, "reason": r, "rule": rule})).collect::<Vec<_>>(),
            "skippedDirectories": self.skipped_dirs.iter().map(|(p, r)| json!({"path": p, "reason": r})).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The size of one file, and the only place detection touches a file at all (a metadata call:
/// never an open, never a read).
fn sized(e: &std::fs::DirEntry, d: &mut Detection, child: &str) -> u64 {
    match e.metadata() {
        Ok(m) => m.len(),
        Err(err) => {
            d.warnings.push(format!("{child}: {err}"));
            0
        }
    }
}

pub fn detect_profile(root: &Path) -> Detection {
    detect_with_limits(root, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_MS)
}

/// The detection itself. `max_entries` and `max_ms` exist so a test can prove the caps work.
///
/// The walk is the whole cost of this function, so it allocates nothing per file: extensions are
/// interned once, a name is lowercased only when it has to be, and the maps that exist for markers
/// and folder hints are touched only when a name can actually matter. The measured cost for 100 000
/// files is printed by `tools/detect_bench.py`.
pub fn detect_with_limits(root: &Path, max_entries: usize, max_ms: u64) -> Detection {
    let t0 = Instant::now();
    let mut d = Detection { root: root.to_string_lossy().to_string(), ..Default::default() };

    // extension (bare, lowercase) -> the profiles that claim it
    let mut claimers: BTreeMap<&'static str, Vec<&'static Profile>> = BTreeMap::new();
    // The names and extensions that can matter to a marker, a folder hint or an anti-marker.
    // Everything else is deliberately not remembered: keeping every name of a 100 000-file folder
    // buys nothing and costs the whole point of the cap.
    let mut marker_names: BTreeSet<String> = BTreeSet::new();
    let mut hint_names: BTreeSet<String> = BTreeSet::new();
    let mut marker_exts: BTreeSet<String> = BTreeSet::new();
    for p in profiles::PROFILES {
        for ext in p.extensions {
            claimers.entry(ext).or_default().push(p);
        }
        for m in p.markers.iter().chain(p.anti_markers.iter()) {
            marker_names.insert(m.to_lowercase());
            if let Some(dirname) = m.strip_suffix('/') {
                hint_names.insert(dirname.to_lowercase());
            } else if m.starts_with('.') && !m.contains('*') {
                marker_exts.insert(m.trim_start_matches('.').to_lowercase());
            }
        }
        for h in p.folder_hints {
            hint_names.insert((*h).to_string());
        }
    }

    let mut claimed: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut claimed_within: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new(); // only marker names
    let mut ext_seen: BTreeSet<String> = BTreeSet::new(); // only marker extensions
    let mut dir_names: BTreeSet<String> = BTreeSet::new(); // only hint or marker directories
    // Interned extensions; id 0 is "(none)".
    let mut ext_ids: BTreeMap<String, u32> = BTreeMap::new();
    let mut ext_names: Vec<String> = vec!["(none)".to_string()];
    let mut ext_files: Vec<usize> = vec![0];
    let mut informative: Vec<(u64, u32)> = Vec::new();
    let mut literal_files: Vec<(String, u64, u32)> = Vec::new();

    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    // One reusable buffer for the relative path: a 100 000-file folder must not build 100 000
    // Strings that are then thrown away.
    let mut child = String::with_capacity(256);
    let mut stop = false;
    while let Some((dir, rel)) = stack.pop() {
        if stop {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                d.warnings.push(format!("{}: {e}", if rel.is_empty() { "." } else { &rel }));
                continue;
            }
        };
        // No sorting here on purpose: the report is built from aggregates, so the order entries are
        // seen in cannot change it, and sorting 100 000 entries costs more than the whole decision.
        for e in rd.flatten() {
            if d.entries >= max_entries || t0.elapsed().as_millis() as u64 > max_ms {
                d.truncated = true;
                stop = true;
                break;
            }
            d.entries += 1;
            let name_cow = e.file_name();
            let name_cow = name_cow.to_string_lossy();
            let name: &str = &name_cow;
            child.clear();
            if !rel.is_empty() {
                child.push_str(&rel);
                child.push('/');
            }
            child.push_str(name);
            let name_lower;
            let name_n: &str = if name.bytes().any(|b| b.is_ascii_uppercase()) {
                name_lower = name.to_ascii_lowercase();
                &name_lower
            } else {
                name
            };
            // `file_type()` needs no system call where the filesystem reports the type with the
            // directory entry; a directory, a link or a device is therefore never stat'ed at all.
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(err) => {
                    d.warnings.push(format!("{child}: {err}"));
                    continue;
                }
            };
            if ft.is_dir() {
                d.directories += 1;
                if hint_names.contains(name_n) || marker_names.contains(name_n) {
                    dir_names.insert(name_n.to_string());
                }
                let dep = profiles::dependency_dir(name);
                if dep || name.starts_with('.') {
                    d.skipped_dirs.push((child.clone(), if dep { "dependency" } else { "hidden" }.to_string()));
                    continue;
                }
                stack.push((e.path(), child.clone()));
                continue;
            }
            if ft.is_symlink() {
                // A link is never followed: reading a target would leave the folder being examined.
                d.symlinks += 1;
                continue;
            }
            if !ft.is_file() {
                // FIFOs, sockets, devices: never opened, only counted.
                d.special += 1;
                continue;
            }
            if let Some((reason, rule)) = profiles::safety_rule(&child) {
                d.safety_excluded.push((child.clone(), reason, rule));
                continue;
            }
            d.files += 1;
            // The extension: a borrowed slice, lowercased only when it has to be.
            let ext_raw = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext,
                _ => "",
            };
            let ext_lower;
            let ext_n: &str = if ext_raw.bytes().any(|b| b.is_ascii_uppercase()) {
                ext_lower = ext_raw.to_ascii_lowercase();
                &ext_lower
            } else {
                ext_raw
            };
            let id: u32 = if ext_n.is_empty() {
                0
            } else {
                match ext_ids.get(ext_n) {
                    Some(i) => *i,
                    None => {
                        let i = ext_names.len() as u32;
                        ext_names.push(ext_n.to_string());
                        ext_files.push(0);
                        ext_ids.insert(ext_n.to_string(), i);
                        i
                    }
                }
            };
            ext_files[id as usize] += 1;
            let named = marker_names.contains(name_n);
            if named {
                *by_name.entry(name_n.to_string()).or_insert(0) += 1;
            }
            if ext_n.is_empty() {
                // A file without an extension can still be named literally by a profile (Makefile);
                // nothing else about it needs its size.
                if named {
                    let size = sized(&e, &mut d, &child);
                    literal_files.push((name_n.to_string(), size, id));
                } else {
                    d.unsized_files += 1;
                }
                continue;
            }
            if marker_exts.contains(ext_n) {
                ext_seen.insert(ext_n.to_string());
            }
            let claimers_here = claimers.get(ext_n);
            // The size is needed only where it can change an answer: a file some profile claims (the
            // size score and the estimate), a media or archive attachment (the 1 GB warning), or a
            // name a profile names literally. Anything else is counted and left unsized, which is
            // what keeps a 100 000-file folder to one system call per file that matters.
            if claimers_here.is_some() || profiles::attachment_ext(ext_n) || named {
                let size = sized(&e, &mut d, &child);
                if named {
                    literal_files.push((name_n.to_string(), size, id));
                }
                if size > LARGE_FILE_BYTES {
                    d.large_files.push((child.clone(), size));
                }
                if let Some(ps) = claimers_here {
                    for p in ps {
                        *claimed.entry(p.id).or_insert(0) += 1;
                        if size <= p.default_max_file_size_kb * 1024 {
                            *claimed_within.entry(p.id).or_insert(0) += 1;
                        }
                    }
                    informative.push((size, id));
                }
            } else {
                d.unsized_files += 1;
            }
        }
    }
    for (i, n) in ext_names.iter().enumerate() {
        if ext_files[i] == 0 {
            continue;
        }
        let key = if i == 0 { "(none)".to_string() } else { format!(".{n}") };
        d.extension_counts.insert(key, ext_files[i]);
    }

    let attributable: usize = informative.len();
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    for (ext, n) in &d.extension_counts {
        let bare = ext.trim_start_matches('.');
        if *ext == "(none)" || claimers.get(bare).is_none() {
            *unknown.entry(ext.clone()).or_insert(0) += *n;
        }
    }
    let mut unknown: Vec<(String, usize)> = unknown.into_iter().collect();
    unknown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    unknown.truncate(20);
    d.unknown_extensions = unknown;

    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // ---- scoring ---------------------------------------------------------------------------
    let mut scores: Vec<Score> = Vec::new();
    for p in profiles::PROFILES {
        let ext_score = if attributable == 0 {
            0.0
        } else {
            *claimed.get(p.id).unwrap_or(&0) as f64 / attributable as f64
        };

        let mut markers_hit: Vec<String> = Vec::new();
        for m in p.markers {
            let ml = m.to_lowercase();
            let hit = match m.strip_suffix('/') {
                Some(dirname) => dir_names.contains(&dirname.to_lowercase()),
                None => {
                    dir_names.contains(&ml)
                        || by_name.contains_key(&ml)
                        || (m.starts_with('.') && ext_seen.contains(&m.trim_start_matches('.').to_lowercase()))
                }
            };
            if hit {
                markers_hit.push((*m).to_string());
            }
        }
        let mut anti_hit: Vec<String> = Vec::new();
        for m in p.anti_markers {
            let ml = m.to_lowercase();
            if dir_names.contains(&ml) || by_name.contains_key(&ml) {
                anti_hit.push((*m).to_string());
            }
        }
        let mut folder_hits: Vec<String> = Vec::new();
        for h in p.folder_hints {
            let hl = h.to_lowercase();
            if hl == root_name || dir_names.contains(&hl) {
                folder_hits.push((*h).to_string());
            }
        }
        let within = *claimed_within.get(p.id).unwrap_or(&0);
        let total_claimed = *claimed.get(p.id).unwrap_or(&0);
        let size_score = if total_claimed == 0 { 0.0 } else { within as f64 / total_claimed as f64 };
        let marker_score = if markers_hit.is_empty() { 0.0 } else { 1.0 };
        let folder_score = if folder_hits.is_empty() { 0.0 } else { 1.0 };

        let mut comps: Vec<(&'static str, f64, f64)> = vec![("extension", W_EXT, ext_score)];
        if !p.markers.is_empty() {
            comps.push(("markers", W_MARKER, marker_score));
        }
        if !p.folder_hints.is_empty() {
            comps.push(("folders", W_FOLDER, folder_score));
        }
        comps.push(("size", W_SIZE, size_score));
        let wsum: f64 = comps.iter().map(|c| c.1).sum();
        let raw: f64 = if wsum > 0.0 { comps.iter().map(|c| c.1 * c.2).sum::<f64>() / wsum } else { 0.0 };
        let penalty = 1.0 - (0.15 * anti_hit.len() as f64).min(0.30);
        let score = (raw * penalty).clamp(0.0, 1.0);
        scores.push(Score {
            id: p.id.to_string(),
            display_name: p.display_name.to_string(),
            confidence: (score * 100.0).round() as u32,
            extension_score: ext_score,
            marker_score,
            folder_score,
            size_score,
            components: comps.iter().map(|c| c.0).collect(),
            markers_hit,
            folder_hits,
            anti_markers_hit: anti_hit,
        });
    }
    scores.sort_by(|a, b| b.confidence.cmp(&a.confidence).then(a.id.cmp(&b.id)));

    let confident = scores.first().map(|s| s.confidence >= LOW_CONFIDENCE).unwrap_or(false);
    d.ambiguous = confident && scores.len() > 1 && scores[0].confidence - scores[1].confidence <= AMBIGUITY_GAP;
    d.suggest_custom = !confident;
    d.markers = scores.first().map(|s| s.markers_hit.clone()).unwrap_or_default();
    d.folder_hints = scores.first().map(|s| s.folder_hits.clone()).unwrap_or_default();
    d.candidates = scores;

    // ---- what the profile would protect -----------------------------------------------------
    let chosen: Option<Profile> = if d.suggest_custom {
        None
    } else {
        d.candidates.first().and_then(|c| profiles::by_id(&c.id)).map(clone_profile)
    };
    if let Some(p) = &chosen {
        let (include, left_out) = profiles::include_globs(p);
        d.include_globs = include.clone();
        d.exclude_globs = profiles::exclude_globs(p);
        d.left_out_media = left_out.clone();
        d.max_file_size_kb = p.default_max_file_size_kb;

        // estimate: the files whose extension the include list covers and that fit the limit, plus
        // the files the profile names literally (package.json, Makefile …), counted once each.
        let allowed_ext: BTreeSet<String> = include
            .iter()
            .filter_map(|g| g.strip_prefix("*.").map(|e| e.to_lowercase()))
            .collect();
        let literal: BTreeSet<String> = include
            .iter()
            .filter(|g| !g.starts_with("*.") && !g.ends_with("/**"))
            .map(|g| g.to_lowercase())
            .collect();
        let limit = p.default_max_file_size_kb.checked_mul(1024).unwrap_or(u64::MAX);
        for (size, id) in &informative {
            if *size > limit {
                continue;
            }
            if allowed_ext.contains(&ext_names[*id as usize]) {
                d.estimated_files += 1;
                d.estimated_archive_bytes += *size;
            }
        }
        for (name, size, id) in &literal_files {
            if *size > limit || !literal.contains(name) {
                continue;
            }
            if allowed_ext.contains(&ext_names[*id as usize]) {
                // already counted above as an extension match
                continue;
            }
            d.estimated_files += 1;
            d.estimated_archive_bytes += *size;
        }
    }

    // ---- what a person needs to know before saying yes --------------------------------------
    // The walk does not sort, so the lists that are printed are sorted here: same input, same report.
    d.large_files.sort();
    d.safety_excluded.sort();
    d.skipped_dirs.sort();
    if !d.large_files.is_empty() {
        let n = d.large_files.len();
        d.warnings.push(if n == 1 {
            "Found 1 file larger than 1 GB. It will be skipped unless you explicitly include large files."
                .to_string()
        } else {
            format!(
                "Found {n} files larger than 1 GB. They will be skipped unless you explicitly include large files."
            )
        });
    }
    if !d.safety_excluded.is_empty() {
        let n = d.safety_excluded.len();
        let secrets = d.safety_excluded.iter().filter(|(_, r, _)| r == crate::filters::R_SECRET).count();
        let temp = d.safety_excluded.iter().filter(|(_, r, _)| r == crate::filters::R_TEMPORARY).count();
        d.warnings.push(format!(
            "{} excluded by the hard-coded safety rules and never included automatically ({secrets} secret, {temp} temporary).",
            if n == 1 { "1 file is".to_string() } else { format!("{n} files are") }
        ));
    }
    if !d.left_out_media.is_empty() {
        d.warnings.push(format!(
            "{} media patterns are not auto-included under this profile (mediaPolicy). Add them explicitly to protect them: {}",
            d.left_out_media.len(),
            d.left_out_media.join(" ")
        ));
    }
    if d.truncated {
        d.warnings.push(format!(
            "the scan stopped early ({} entries, {} ms) — the numbers below describe only the part that was seen",
            d.entries, max_ms
        ));
    }
    if d.suggest_custom {
        d.warnings.push(
            "no profile reached a convincing confidence — pick `custom` and name the extensions yourself".to_string(),
        );
    } else if d.ambiguous {
        d.warnings.push(format!(
            "{} and {} are within {} points of each other: choose one (or `edit`)",
            d.candidates[0].id, d.candidates[1].id, AMBIGUITY_GAP
        ));
    }
    if !d.skipped_dirs.is_empty() {
        let n = d.skipped_dirs.len();
        d.warnings.push(if n == 1 {
            "1 directory was not descended into (a dependency or hidden folder)".to_string()
        } else {
            format!("{n} directories were not descended into (dependencies and hidden folders)")
        });
    }
    d.elapsed_ms = t0.elapsed().as_millis() as i64;
    d
}

fn clone_profile(p: &'static Profile) -> Profile {
    Profile {
        id: p.id,
        display_name: p.display_name,
        extensions: p.extensions,
        markers: p.markers,
        anti_markers: p.anti_markers,
        folder_hints: p.folder_hints,
        default_max_file_size_kb: p.default_max_file_size_kb,
        binary_policy: p.binary_policy,
        media_policy: p.media_policy,
        exclude_globs: p.exclude_globs,
    }
}

/// Human-readable size for the report.
pub fn human(bytes: u64) -> String {
    util::human_size(bytes)
}
