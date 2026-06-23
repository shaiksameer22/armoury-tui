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
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file()))
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

    // -- read_text tests ---------------------------------------------------

    #[test]
    fn test_read_text_trims_whitespace() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_rt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trimme.txt");
        std::fs::write(&file, "  hello  \n").unwrap();
        assert_eq!(read_text(&file), Some("hello".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_text_nonexistent() {
        assert_eq!(
            read_text("/tmp/this_path_does_not_exist_armoury_123456"),
            None
        );
    }

    // -- read_int tests ----------------------------------------------------

    #[test]
    fn test_read_int_normal() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_ri_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("int_val.txt");
        std::fs::write(&file, "42000").unwrap();
        assert_eq!(read_int(&file), Some(42000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_int_empty() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_rie_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("empty.txt");
        std::fs::write(&file, "").unwrap();
        assert_eq!(read_int(&file), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- read_milli tests --------------------------------------------------

    #[test]
    fn test_read_milli_conversion() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_rm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("milli.txt");
        std::fs::write(&file, "61000").unwrap();
        assert_eq!(read_milli(&file), Some(61.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- read_float tests --------------------------------------------------

    #[test]
    fn test_read_float_with_units() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_rf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("float.txt");
        std::fs::write(&file, "3.14 units").unwrap();
        let v = read_float(&file);
        assert!(v.is_some());
        assert!((v.unwrap() - 3.14).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- glob_in tests -----------------------------------------------------

    #[test]
    fn test_glob_in_matches_prefix_suffix() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_gl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Create files: temp1_input, temp2_input, fan1_input
        std::fs::write(dir.join("temp1_input"), "1").unwrap();
        std::fs::write(dir.join("temp2_input"), "2").unwrap();
        std::fs::write(dir.join("fan1_input"), "3").unwrap();

        let results = glob_in(&dir, "temp", "_input");
        assert_eq!(results.len(), 2);
        // Verify they are sorted
        let names: Vec<String> = results
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["temp1_input", "temp2_input"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_glob_in_no_matches() {
        let dir = std::env::temp_dir().join(format!("sysfs_test_gln_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fan1_input"), "1").unwrap();
        let results = glob_in(&dir, "temp", "_input");
        assert!(results.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- which tests -------------------------------------------------------

    #[test]
    fn test_which_cargo() {
        // cargo is always available in a Rust test environment
        assert!(which("cargo"));
    }

    #[test]
    fn test_which_nonexistent() {
        assert!(!which("nonexistent_binary_12345"));
    }
}
