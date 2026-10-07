//! Filters: what is copied, what is skipped and why. Every skip carries a reason and the rule that
//! caused it, so `pl why` and `status --skipped` can explain themselves.

use crate::glob::{matches_anywhere, IgnoreRules};
use serde_json::{Map, Value};

pub const R_SECRET: &str = "secret";
pub const R_TOO_LARGE: &str = "too_large";
pub const R_BINARY: &str = "binary";
pub const R_IGNORED_DIR: &str = "ignored_dir";
pub const R_HIDDEN: &str = "hidden";
pub const R_RULE: &str = "rule";
pub const R_UNREADABLE: &str = "unreadable";
pub const R_SYMLINK_ARCHIVE: &str = "symlink_to_archive";
/// Round 291: the file is not one the active preset protects (see SPEC §6.17).
pub const R_NOT_IN_PRESET: &str = "not_in_preset";
/// Round 291: editor crutches and temporary files, never tracked whatever the preset says.
pub const R_TEMPORARY: &str = "temporary";

pub const L_RULE: &str = "rule";
pub const DEFAULT_IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    "coverage",
    ".next",
    ".turbo",
    "vendor",
    "Pods",
    "DerivedData",
    ".build",
    ".gradle",
    "__pycache__",
    ".venv",
    "venv",
    ".git",
];

pub const DEFAULT_SECRETS: &[&str] = &[
    ".env",
    ".env.*",
    "secrets.*",
    "id_rsa",
    "id_rsa.*",
    "id_ed25519",
    "id_ed25519.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.p7b",
    "*.crt",
    "*.cer",
    "*.keystore",
    "*.jks",
    "credentials.json",
    "service-account*.json",
    ".aws/credentials",
    ".docker/config.json",
    ".kube/config",
    ".ssh/*",
    ".netrc",
    ".npmrc",
    ".pgpass",
];

pub const DEFAULT_BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif", "webp", "heic", "ico", "psd", "ai", "sketch",
    "mp4", "mov", "avi", "mkv", "webm", "flv", "wmv", "m4v", "mp3", "wav", "flac", "aac", "ogg",
    "m4a", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "tgz", "zst", "dmg", "iso", "ttf", "otf",
    "woff", "woff2", "eot", "exe", "dll", "so", "dylib", "bin", "o", "a", "obj", "lib", "class",
    "jar", "wasm", "elf", "db", "sqlite", "sqlite3", "mdb", "pdf", "doc", "docx", "xls", "xlsx",
    "ppt", "pptx", "epub",
];

pub const DEFAULT_ALLOWED_HIDDEN: &[&str] = &[
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    ".eslintrc",
    ".eslintrc.json",
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.js",
    ".dockerignore",
    ".github/**",
    ".nvmrc",
    ".python-version",
    ".gitkeep",
];

#[derive(Clone, Debug)]
pub struct FilterConfig {
    pub profile: String,
    pub max_file_size: u64,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub ignored_dirs: Vec<String>,
    pub include_secrets: bool,
    pub use_gitignore: bool,
    pub secret_patterns: Vec<String>,
    pub binary_exts: Vec<String>,
    pub allowed_hidden: Vec<String>,
    /// `force` (default): `include` adds paths on top of the profile. `allow`: `include` is the
    /// whole list — a file matching nothing in it is not tracked. A preset writes `allow`.
    pub filter_mode: String,
    /// The preset this filter came from, used as the rule text of `not_in_preset`.
    pub preset_id: String,
    /// `settings.includeLargeFiles`: the only way past the size limit (SPEC §6.17).
    pub include_large_files: bool,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self::for_profile("source", &Map::new())
    }
}

impl FilterConfig {
    pub fn for_profile(profile: &str, settings: &Map<String, Value>) -> Self {
        let default_limit_kb: u64 = if profile == "all" { 50 * 1024 } else { 2 * 1024 };
        let max_file_size = settings
            .get("maxFileSizeKb")
            .and_then(|v| v.as_u64())
            .unwrap_or(default_limit_kb)
            * 1024;
        let list = |key: &str, def: &[&str]| -> Vec<String> {
            match settings.get(key).and_then(|v| v.as_array()) {
                Some(a) if !a.is_empty() => {
                    a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
                }
                _ => def.iter().map(|s| s.to_string()).collect(),
            }
        };
        let extra = |key: &str| -> Vec<String> {
            settings
                .get(key)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default()
        };
        Self {
            profile: profile.to_string(),
            max_file_size,
            include: extra("include"),
            exclude: extra("exclude"),
            ignored_dirs: {
                let mut v = list("ignoredFolders", DEFAULT_IGNORED_DIRS);
                for d in DEFAULT_IGNORED_DIRS {
                    if !v.iter().any(|x| x == d) {
                        v.push((*d).to_string());
                    }
                }
                v
            },
            include_secrets: settings.get("includeSecrets").and_then(|v| v.as_bool()).unwrap_or(false),
            use_gitignore: settings.get("useGitignore").and_then(|v| v.as_bool()).unwrap_or(false),
            secret_patterns: list("secretPatterns", DEFAULT_SECRETS),
            binary_exts: list("binaryExtensions", DEFAULT_BINARY_EXTS),
            allowed_hidden: list("allowedHidden", DEFAULT_ALLOWED_HIDDEN),
            filter_mode: settings
                .get("filterMode")
                .and_then(|v| v.as_str())
                .unwrap_or("force")
                .to_string(),
            preset_id: settings
                .get("presetId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    settings
                        .get("preset")
                        .and_then(|v| v.get("id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default(),
            include_large_files: settings
                .get("includeLargeFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }

    pub fn is_allow_list(&self) -> bool {
        self.filter_mode == "allow"
    }

    pub fn is_all(&self) -> bool {
        self.profile == "all"
    }

    /// Decision for a directory: may we descend into it?
    pub fn decide_dir(
        &self,
        rel: &str,
        archive_prefixes: &[String],
        ignore: &IgnoreRules,
        gitignore: &IgnoreRules,
    ) -> Decision {
        if rel.is_empty() {
            return Decision::track();
        }
        for pre in archive_prefixes {
            if !pre.is_empty() && (rel == pre.as_str() || rel.starts_with(&format!("{pre}/"))) {
                return Decision::skip(R_IGNORED_DIR, "the archive lives inside the project");
            }
        }
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if self.ignored_dirs.iter().any(|d| d == name || d == rel) {
            return Decision::skip(R_IGNORED_DIR, name);
        }
        if let Some(rule) = ignore.ignored(rel) {
            return Decision::skip(R_RULE, &rule);
        }
        if self.use_gitignore {
            if let Some(rule) = gitignore.ignored(rel) {
                return Decision::skip(R_RULE, &rule);
            }
        }
        if self.exclude.iter().any(|p| matches_anywhere(p, rel)) {
            let rule = self.exclude.iter().find(|p| matches_anywhere(p, rel)).cloned().unwrap_or_default();
            return Decision::skip(R_RULE, &rule);
        }
        if !self.is_all() && name.starts_with('.') && !self.allowed_hidden.iter().any(|p| matches_anywhere(p, rel)) {
            return Decision::skip(R_HIDDEN, name);
        }
        Decision::track()
    }

    /// Decision for a file.
    pub fn decide_file(
        &self,
        rel: &str,
        size: u64,
        ignore: &IgnoreRules,
        gitignore: &IgnoreRules,
    ) -> Decision {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let forced = self.include.iter().any(|p| matches_anywhere(p, rel));

        // Hard rules first: no preset, no include and no flag puts a temporary file or a secret in
        // the archive automatically. `--include-secrets` is the one explicit way past the secrets.
        if let Some(pat) = crate::profiles::temporary_rule(rel) {
            return Decision::skip(R_TEMPORARY, &pat);
        }
        if !self.include_secrets {
            if let Some(pat) = self.secret_patterns.iter().find(|p| matches_anywhere(p, rel)) {
                return Decision::skip(R_SECRET, pat);
            }
        }

        // An allow-list filter (what a preset writes) protects the listed patterns and nothing else.
        if self.is_allow_list() && !forced {
            let rule = if self.preset_id.is_empty() { "preset".to_string() } else { self.preset_id.clone() };
            return Decision::skip(R_NOT_IN_PRESET, &rule);
        }

        if self.exclude.iter().any(|p| matches_anywhere(p, rel)) && !forced {
            let rule = self.exclude.iter().find(|p| matches_anywhere(p, rel)).cloned().unwrap_or_default();
            return Decision::skip(R_RULE, &rule);
        }
        if !forced {
            if let Some(rule) = ignore.ignored(rel) {
                return Decision::skip(R_RULE, &rule);
            }
            if self.use_gitignore {
                if let Some(rule) = gitignore.ignored(rel) {
                    return Decision::skip(R_RULE, &rule);
                }
            }
        }
        let hidden = name.starts_with('.');
        let allowed_hidden = self.allowed_hidden.iter().any(|p| matches_anywhere(p, rel));
        if hidden && !self.is_all() && !allowed_hidden && !forced {
            return Decision::skip(R_HIDDEN, name);
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        let binary = !ext.is_empty() && self.binary_exts.iter().any(|e| e == &ext);
        if binary && !self.is_all() && !forced {
            return Decision::skip(R_BINARY, &format!(".{ext}"));
        }
        // The size limit is a real limit: an include list is not a licence for a 2 GB file. Since
        // round 291 only `includeLargeFiles` lifts it (SPEC §6.17, AT-47).
        if size > self.max_file_size && !self.include_large_files {
            return Decision::skip(R_TOO_LARGE, &format!("> {} KB", self.max_file_size / 1024));
        }
        Decision::track()
    }
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub track: bool,
    pub reason: Option<String>,
    pub rule: Option<String>,
}

impl Decision {
    pub fn track() -> Self {
        Decision { track: true, reason: None, rule: None }
    }
    pub fn skip(reason: &str, rule: &str) -> Self {
        Decision { track: false, reason: Some(reason.to_string()), rule: Some(rule.to_string()) }
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}
