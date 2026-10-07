//! Profiles: the built-in knowledge of what a folder of files probably *is*, and the hard-coded
//! safety rules that no profile, no flag and no preset can break (SPEC §6.17).
//!
//! A profile is a suggestion, never an authority. It says which extensions are worth protecting,
//! which file names are evidence for it, which folder names hint at it, and what size limit to
//! apply. Detection (see `detect.rs`) uses the metadata of these tables only: names, sizes and
//! extensions. No file content is ever read to decide what a folder is.

use crate::filters::{DEFAULT_IGNORED_DIRS, DEFAULT_SECRETS};
use crate::glob::matches_anywhere;
use std::path::Path;

pub const POLICY_EXCLUDE: &str = "exclude";
pub const POLICY_ASK: &str = "ask";
pub const POLICY_INCLUDE: &str = "include";

/// Extensions that are *media attachments* rather than the work itself. A profile with
/// `mediaPolicy = "exclude"` never writes them into its include list; `"ask"` reports them and
/// leaves them out until the user says otherwise; `"include"` protects them like anything else.
pub const MEDIA_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "heic", "ico", "psd", "psb", "ai",
    "eps", "svg", "xd", "fig", "sketch", "kra", "xcf", "afdesign", "afphoto", "arw", "cr2", "cr3",
    "nef", "dng", "orf", "rw2", "raf", "mp4", "mov", "avi", "mkv", "webm", "wmv", "m4v", "mxf",
    "mpg", "mpeg", "mp3", "wav", "flac", "aac", "ogg", "m4a", "aiff", "aif", "mid", "midi",
];

/// Temporary and editor-crutch files. Hard rule: never tracked, whatever the profile says.
pub const TEMP_PATTERNS: &[&str] = &[
    "*.tmp", "*.temp", "~$*", ".DS_Store", "Thumbs.db", "*.swp", "*.swo", "*.bak", "*.orig", "*~",
];

/// Absolute prefixes that are never a project root. Adding them is refused, not silently allowed.
pub const SYSTEM_PREFIXES: &[&str] =
    &["/System", "/Windows", "/etc", "/usr", "/bin", "/sbin", "/var", "/proc", "/sys", "/dev"];

pub struct Profile {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Extensions without the dot, lowercase.
    pub extensions: &'static [&'static str],
    /// File names that are strong evidence (> = directory name, a leading dot = extension marker).
    pub markers: &'static [&'static str],
    /// File names that argue *against* this profile.
    pub anti_markers: &'static [&'static str],
    /// Directory names that hint at this profile.
    pub folder_hints: &'static [&'static str],
    pub default_max_file_size_kb: u64,
    pub binary_policy: &'static str,
    pub media_policy: &'static str,
    /// Patterns excluded for this profile (temporary files, executables, archives, media it does
    /// not protect).
    pub exclude_globs: &'static [&'static str],
}

const COMMON_EXCLUDE: &[&str] = &[
    "*.tmp", "*.temp", "~$*", ".DS_Store", "Thumbs.db", "*.swp", "*.swo", "*.bak", "*.orig",
    "*.zip", "*.tar", "*.gz", "*.tgz", "*.bz2", "*.xz", "*.7z", "*.rar", "*.zst", "*.dmg", "*.iso",
    "*.exe", "*.dll", "*.so", "*.dylib", "*.bin", "*.o", "*.a", "*.class", "*.jar", "*.wasm",
    "*.msi", "*.app",
];

const MEDIA_EXCLUDE: &[&str] = &[
    "*.tmp", "*.temp", "~$*", ".DS_Store", "Thumbs.db", "*.swp", "*.bak", "*.zip", "*.tar",
    "*.gz", "*.7z", "*.rar", "*.exe", "*.dll", "*.so", "*.dylib", "*.iso",
];

pub static PROFILES: &[Profile] = &[
    Profile {
        id: "office",
        display_name: "Office / Documents",
        extensions: &[
            "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "odt", "ods", "odp", "rtf", "txt",
            "csv",
        ],
        markers: &[],
        anti_markers: &["package.json", "Cargo.toml", "go.mod", "pyproject.toml", "requirements.txt", "Dockerfile"],
        folder_hints: &["documents", "document", "docs", "office", "reports", "invoices", "contracts", "paperwork", "letters"],
        default_max_file_size_kb: 102_400,
        binary_policy: POLICY_EXCLUDE,
        media_policy: POLICY_EXCLUDE,
        exclude_globs: COMMON_EXCLUDE,
    },
    Profile {
        id: "developer",
        display_name: "Developer / Source code",
        extensions: &[
            "ts", "tsx", "js", "jsx", "py", "rs", "go", "java", "kt", "swift", "c", "h", "cpp", "cs",
            "rb", "php", "sql", "sh", "json", "yaml", "yml", "toml", "html", "css", "scss", "md",
        ],
        markers: &["package.json", "Cargo.toml", "go.mod", "pyproject.toml", "requirements.txt", "Makefile", "Dockerfile", ".git"],
        anti_markers: &["layout.psd"],
        folder_hints: &["src", "lib", "app", "apps", "server", "api", "backend", "frontend", "tests", "test"],
        default_max_file_size_kb: 10_240,
        binary_policy: POLICY_EXCLUDE,
        media_policy: POLICY_EXCLUDE,
        exclude_globs: COMMON_EXCLUDE,
    },
    Profile {
        id: "designer",
        display_name: "Designer / Visual work",
        extensions: &[
            "psd", "psb", "ai", "eps", "fig", "sketch", "xd", "svg", "png", "jpg", "jpeg", "webp",
            "tiff", "tif", "gif", "bmp", "afdesign", "afphoto", "kra", "xcf",
        ],
        markers: &[".psd", ".fig", ".sketch"],
        anti_markers: &["package.json"],
        folder_hints: &["design", "designs", "art", "artwork", "mockups", "ui", "ux", "brand", "branding", "assets"],
        default_max_file_size_kb: 1_048_576,
        binary_policy: POLICY_INCLUDE,
        media_policy: POLICY_INCLUDE,
        exclude_globs: MEDIA_EXCLUDE,
    },
    Profile {
        id: "writer",
        display_name: "Writer / Manuscripts",
        extensions: &["md", "markdown", "txt", "rtf", "doc", "docx", "odt", "pages", "tex", "bib", "epub"],
        markers: &[],
        anti_markers: &["package.json", "Cargo.toml"],
        folder_hints: &["writing", "manuscript", "manuscripts", "drafts", "draft", "book", "chapters", "notes"],
        default_max_file_size_kb: 20_480,
        binary_policy: POLICY_EXCLUDE,
        media_policy: POLICY_EXCLUDE,
        exclude_globs: COMMON_EXCLUDE,
    },
    Profile {
        id: "photographer",
        display_name: "Photographer / Raw images",
        extensions: &[
            "arw", "cr2", "cr3", "nef", "dng", "orf", "rw2", "raf", "jpg", "jpeg", "heic", "xmp",
            "lrcat", "lrtemplate",
        ],
        markers: &[".lrcat", ".xmp"],
        anti_markers: &["package.json"],
        folder_hints: &["photos", "photo", "photography", "shoot", "shoots", "raw", "lightroom", "captures", "pictures", "catalog"],
        default_max_file_size_kb: 204_800,
        binary_policy: POLICY_INCLUDE,
        media_policy: POLICY_INCLUDE,
        exclude_globs: MEDIA_EXCLUDE,
    },
    Profile {
        id: "video",
        display_name: "Video / Post-production",
        extensions: &[
            "prproj", "drp", "fcpxml", "aep", "mogrt", "edl", "srt", "vtt", "mp4", "mov", "avi",
            "mkv", "mxf", "wav", "mp3", "aac",
        ],
        markers: &[".prproj", ".drp", ".fcpxml", ".aep"],
        anti_markers: &["package.json"],
        folder_hints: &["video", "footage", "edits", "renders", "timeline", "b-roll", "premiere", "resolve"],
        default_max_file_size_kb: 512_000,
        binary_policy: POLICY_INCLUDE,
        media_policy: POLICY_ASK,
        exclude_globs: MEDIA_EXCLUDE,
    },
    Profile {
        id: "3d",
        display_name: "3D / Models and scenes",
        extensions: &[
            "blend", "ma", "mb", "c4d", "max", "obj", "fbx", "gltf", "glb", "usd", "usda", "usdc",
            "usdz", "stl", "step", "iges",
        ],
        markers: &[".blend", ".ma", ".mb", ".c4d"],
        anti_markers: &["package.json"],
        folder_hints: &["models", "scenes", "textures", "rigs", "3d", "geometry", "materials"],
        default_max_file_size_kb: 1_048_576,
        binary_policy: POLICY_INCLUDE,
        media_policy: POLICY_INCLUDE,
        exclude_globs: MEDIA_EXCLUDE,
    },
    Profile {
        id: "data",
        display_name: "Data / Analysis",
        extensions: &[
            "csv", "tsv", "parquet", "orc", "feather", "ipynb", "r", "rmd", "py", "sql", "json",
            "xlsx", "sqlite", "db", "duckdb",
        ],
        markers: &[".ipynb", "requirements.txt", "pyproject.toml"],
        anti_markers: &["package.json", "Dockerfile"],
        folder_hints: &["data", "datasets", "dataset", "analysis", "notebooks", "warehouse", "etl"],
        default_max_file_size_kb: 512_000,
        binary_policy: POLICY_ASK,
        media_policy: POLICY_EXCLUDE,
        exclude_globs: COMMON_EXCLUDE,
    },
    Profile {
        id: "audio",
        display_name: "Audio / Music production",
        extensions: &[
            "wav", "mp3", "flac", "aiff", "aif", "ogg", "m4a", "mid", "midi", "als", "rpp",
            "dawproject", "aup3", "ptx", "sng",
        ],
        markers: &[".als", ".rpp", ".dawproject"],
        anti_markers: &["package.json"],
        folder_hints: &["audio", "music", "sessions", "stems", "mixes", "recordings", "sound", "mastering"],
        default_max_file_size_kb: 1_048_576,
        binary_policy: POLICY_INCLUDE,
        media_policy: POLICY_INCLUDE,
        exclude_globs: MEDIA_EXCLUDE,
    },
    Profile {
        id: "devops",
        display_name: "DevOps / Configuration",
        extensions: &[
            "conf", "cfg", "ini", "yaml", "yml", "toml", "json", "xml", "tf", "hcl", "sh", "bash",
            "zsh", "ps1", "service", "timer", "env.example",
        ],
        markers: &["Dockerfile", "docker-compose.yml", "Makefile", "ansible/", "terraform/", "k8s/"],
        anti_markers: &["layout.psd"],
        folder_hints: &["infra", "infrastructure", "terraform", "ansible", "k8s", "kubernetes", "helm", "deploy", "ops", "systemd", "configs"],
        default_max_file_size_kb: 5_120,
        binary_policy: POLICY_EXCLUDE,
        media_policy: POLICY_EXCLUDE,
        exclude_globs: COMMON_EXCLUDE,
    },
];

/// The built-in ids, in table order.
pub fn ids() -> Vec<&'static str> {
    PROFILES.iter().map(|p| p.id).collect()
}

pub fn by_id(id: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.id == id)
}

/// The profile used by `--preset custom`: the user names the extensions.
pub fn custom_profile() -> Profile {
    Profile {
        id: "custom",
        display_name: "Custom",
        extensions: &[],
        markers: &[],
        anti_markers: &[],
        folder_hints: &[],
        default_max_file_size_kb: 10_240,
        binary_policy: POLICY_ASK,
        media_policy: POLICY_ASK,
        exclude_globs: COMMON_EXCLUDE,
    }
}

pub fn is_media_ext(ext: &str) -> bool {
    MEDIA_EXTS.contains(&ext)
}

/// Archive and executable attachments: never the work, always worth knowing the size of.
pub const ATTACHMENT_EXTS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "dmg", "iso", "exe", "dll", "so",
    "dylib", "bin", "o", "a", "class", "jar", "wasm", "msi", "app", "db", "sqlite", "sqlite3", "mdb",
];

pub fn attachment_ext(ext: &str) -> bool {
    ATTACHMENT_EXTS.contains(&ext) || MEDIA_EXTS.contains(&ext)
}

/// The include globs a profile contributes, given its policies.
///
/// Media extensions are left out unless the profile's `mediaPolicy` is `include`; the ones left out
/// are returned so the caller can say so instead of quietly dropping them. Returns
/// `(include, left_out)`.
pub fn include_globs(p: &Profile) -> (Vec<String>, Vec<String>) {
    let mut include: Vec<String> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for ext in p.extensions {
        if is_media_ext(ext) && p.media_policy != POLICY_INCLUDE {
            left_out.push(format!("*.{ext}"));
            continue;
        }
        include.push(format!("*.{ext}"));
    }
    for m in p.markers {
        if let Some(name) = m.strip_suffix('/') {
            include.push(format!("{name}/**"));
        } else if m.starts_with('.') && !m.contains('*') {
            // an extension marker: covered by the core extensions above
            let ext = m.trim_start_matches('.');
            let glob = format!("*.{ext}");
            if !include.iter().any(|x| x == &glob) {
                include.push(glob);
            }
        } else {
            include.push((*m).to_string());
        }
    }
    include.sort();
    include.dedup();
    left_out.sort();
    left_out.dedup();
    (include, left_out)
}

pub fn exclude_globs(p: &Profile) -> Vec<String> {
    let mut v: Vec<String> = p.exclude_globs.iter().map(|s| s.to_string()).collect();
    v.sort();
    v.dedup();
    v
}

/// `*.ext`, lowercased with the dot — the key used to count extensions. Empty for a file with no
/// extension.
pub fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => format!(".{}", ext.to_lowercase()),
        _ => String::new(),
    }
}

/// Extensions without the dot, for classification.
pub fn ext_bare(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// Which built-in profiles claim this extension (without the dot).
pub fn claimers(ext: &str) -> Vec<&'static str> {
    if ext.is_empty() {
        return Vec::new();
    }
    PROFILES
        .iter()
        .filter(|p| p.extensions.iter().any(|e| *e == ext))
        .map(|p| p.id)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Hard-coded safety rules. Decided here and nowhere else: no preset, no `--preset` value and no
// edit can put one of these into the archive automatically.
// ---------------------------------------------------------------------------------------------

/// `None` = safe; `Some(rule)` = never tracked automatically.
pub fn secret_rule(rel: &str, include_secrets: bool) -> Option<String> {
    if include_secrets {
        return None;
    }
    DEFAULT_SECRETS.iter().find(|p| matches_anywhere(p, rel)).map(|p| (*p).to_string())
}

pub fn dependency_dir(name: &str) -> bool {
    DEFAULT_IGNORED_DIRS.iter().any(|d| *d == name)
}

pub fn temporary_rule(rel: &str) -> Option<String> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    TEMP_PATTERNS.iter().find(|p| matches_anywhere(p, name)).map(|p| (*p).to_string())
}

/// Is this root one of the system locations that must never be protected as a project?
pub fn system_root(path: &Path) -> Option<String> {
    let s = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = s.to_string_lossy();
    for pre in SYSTEM_PREFIXES {
        if s == *pre || s.starts_with(&format!("{pre}/")) {
            return Some((*pre).to_string());
        }
    }
    None
}

/// One check for every rule that outranks a profile: `(reason, rule)`.
pub fn safety_rule(rel: &str) -> Option<(String, String)> {
    if let Some(rule) = temporary_rule(rel) {
        return Some((crate::filters::R_TEMPORARY.into(), rule));
    }
    if let Some(rule) = secret_rule(rel, false) {
        return Some((crate::filters::R_SECRET.into(), rule));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_declares_something() {
        assert_eq!(PROFILES.len(), 10, "ten built-in profiles");
        for p in PROFILES {
            assert!(!p.extensions.is_empty(), "{} has no extensions", p.id);
            assert!(p.default_max_file_size_kb > 0, "{} has no size limit", p.id);
            assert!(by_id(p.id).is_some());
        }
        let mut ids = ids();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 10);
    }

    #[test]
    fn media_is_left_out_unless_the_profile_protects_it() {
        let (inc, out) = include_globs(by_id("office").unwrap());
        assert!(inc.contains(&"*.docx".to_string()));
        assert!(!inc.iter().any(|g| g == "*.mp4"));
        assert!(out.is_empty(), "the office profile lists no media of its own");
        let (inc_v, out_v) = include_globs(by_id("video").unwrap());
        assert!(inc_v.contains(&"*.prproj".to_string()), "project files are the work");
        assert!(out_v.contains(&"*.mp4".to_string()), "media is reported, not silently dropped");
        let (inc_p, out_p) = include_globs(by_id("photographer").unwrap());
        assert!(inc_p.contains(&"*.arw".to_string()));
        assert!(out_p.is_empty(), "the photographer profile protects its media");
    }

    #[test]
    fn safety_rules_do_not_depend_on_the_profile() {
        assert_eq!(secret_rule(".env", false).as_deref(), Some(".env"));
        assert_eq!(secret_rule(".env", true), None);
        assert_eq!(temporary_rule("a/b/cache.tmp").as_deref(), Some("*.tmp"));
        assert!(dependency_dir("node_modules"));
        assert!(!dependency_dir("src"));
        assert!(system_root(Path::new("/etc")).is_some());
    }
}
