//! Content store: blobs addressed by sha256, quarantine, verification helpers.

use crate::util;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn blob_path(blobs_dir: &Path, hash: &str) -> PathBuf {
    let (a, b) = if hash.len() >= 4 { (&hash[0..2], &hash[2..4]) } else { ("00", "00") };
    blobs_dir.join(a).join(b).join(hash)
}

pub struct Store {
    pub blobs_dir: PathBuf,
    pub tmp_dir: PathBuf,
}

impl Store {
    pub fn new(blobs_dir: &Path, tmp_dir: &Path) -> Self {
        Store { blobs_dir: blobs_dir.to_path_buf(), tmp_dir: tmp_dir.to_path_buf() }
    }

    pub fn has(&self, hash: &str) -> bool {
        blob_path(&self.blobs_dir, hash).is_file()
    }

    /// Write a blob: tmp on the same volume → fsync → rename. An existing blob is never rewritten.
    /// Returns true when the blob was created for the first time.
    pub fn put(&self, hash: &str, data: &[u8]) -> Result<bool, String> {
        let dst = blob_path(&self.blobs_dir, hash);
        if dst.is_file() {
            return Ok(false);
        }
        fs::create_dir_all(&self.tmp_dir).map_err(|e| e.to_string())?;
        let tmp = self
            .tmp_dir
            .join(format!("blob-{}-{}", hash.get(..16).unwrap_or(hash), std::process::id()));
        {
            let mut f = fs::File::create(&tmp).map_err(|e| format!("tmp {}: {e}", tmp.display()))?;
            f.write_all(data).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        let parent = dst.parent().unwrap_or(&self.blobs_dir);
        if let Err(e) = fs::create_dir_all(parent) {
            let _ = fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        fs::rename(&tmp, &dst).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })?;
        util::sync_dir(parent);
        util::set_file_readonly(&dst);
        Ok(true)
    }

    /// Read and verify contents. A mismatch is an error: nothing is written from a bad blob.
    pub fn read_verified(&self, hash: &str) -> Result<Vec<u8>, String> {
        let p = blob_path(&self.blobs_dir, hash);
        let data = fs::read(&p).map_err(|e| format!("blob {} is unreadable: {e}", p.display()))?;
        let got = sha256_bytes(&data);
        if got != hash {
            return Err(format!("blob {hash} is corrupted: sha256 {got}"));
        }
        Ok(data)
    }

    pub fn size(&self, hash: &str) -> Option<u64> {
        fs::metadata(blob_path(&self.blobs_dir, hash)).ok().map(|m| m.len())
    }

    /// Move a corrupted blob into quarantine — the bytes are never thrown away.
    pub fn quarantine(&self, hash: &str, quarantine_dir: &Path) -> Result<PathBuf, String> {
        fs::create_dir_all(quarantine_dir).map_err(|e| e.to_string())?;
        let src = blob_path(&self.blobs_dir, hash);
        let dst = quarantine_dir.join(hash);
        fs::rename(&src, &dst).map_err(|e| format!("quarantine {}: {e}", src.display()))?;
        Ok(dst)
    }

    /// Every blob present in the store.
    pub fn list_all(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![self.blobs_dir.clone()];
        while let Some(d) = stack.pop() {
            let rd = match fs::read_dir(&d) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if is_sha256_hex(name) {
                        let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push((name.to_string(), sz));
                    }
                }
            }
        }
        out.sort();
        out
    }
}
