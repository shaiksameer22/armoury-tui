//! Low-level sysfs / procfs read primitives.
//!
//! Reads are deliberately *total*: a missing file, a permission error or a
//! malformed value never panics — it returns `None`. The refresh loop calls
//! these many times per tick, so one unreadable node must never bring the UI
//! down. Port of `sysfs.py`.

use std::fs;
use std::path::{Path, PathBuf};

/// Stripped contents of `path`, or `None` on any failure.
pub fn read_text(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Parse a string as an integer, tolerating trailing units and `0x` hex —
/// mirrors Python's `int(raw.split()[0], 0)` fallback. Pure, so it's unit-tested.
pub fn parse_int_tolerant(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(v) = raw.parse::<i64>() {
        return Some(v);
    }
    let tok = raw.split_whitespace().next()?;
    if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    tok.parse::<i64>().ok()
}

/// Read `path` as an integer (e.g. a millidegree / RPM node).
pub fn read_int(path: impl AsRef<Path>) -> Option<i64> {
    read_text(path).and_then(|raw| parse_int_tolerant(&raw))
}

/// Read `path` as a float.
pub fn read_float(path: impl AsRef<Path>) -> Option<f64> {
    read_text(path).and_then(|raw| raw.split_whitespace().next()?.parse::<f64>().ok())
}

/// Read a sysfs "millis" node (millidegrees / micro-units ÷ 1000) as a float —
/// the single most common conversion in the tool (hwmon temps, power-supply).
pub fn read_milli(path: impl AsRef<Path>) -> Option<f64> {
    read_int(path).map(|v| v as f64 / 1000.0)
}

#[allow(dead_code)] // used by the Phase B controller's fallback sysfs writes
pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

/// Sorted directory entries, swallowing errors (empty vec on failure).
pub fn list_dir(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Entries of `dir` whose file name starts with `prefix` and ends with `suffix`,
/// sorted. Replaces the sysfs globs (`temp*_input`, `fan*_input`, `hwmon*`).
pub fn glob_in(dir: impl AsRef<Path>, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    list_dir(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(suffix))
                .unwrap_or(false)
        })
        .collect()
}

/// `true` if a command is found on `$PATH` (replaces `shutil.which`).
pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerant_int_plain() {
        assert_eq!(parse_int_tolerant("42"), Some(42));
        assert_eq!(parse_int_tolerant("  -7\n"), Some(-7));
    }

    #[test]
    fn tolerant_int_units_and_hex() {
        assert_eq!(parse_int_tolerant("3500 rpm"), Some(3500));
        assert_eq!(parse_int_tolerant("0x1f"), Some(31));
        assert_eq!(parse_int_tolerant("garbage"), None);
        assert_eq!(parse_int_tolerant(""), None);
    }
}
