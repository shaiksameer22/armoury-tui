//! Integration tests for armoury-tui's headless CLI modes.
//!
//! Each test runs the compiled binary as a subprocess and asserts on exit code,
//! stdout/stderr content, and (for --json) structural validity.  Tests that
//! depend on live hardware telemetry (--probe, --once, --json) check *structure*
//! rather than specific values so they pass on any Linux box.

use std::process::Command;

/// Build a `Command` pointing at the compiled binary (resolved at compile time
/// by Cargo for integration tests under `tests/`).
fn armoury() -> Command {
    Command::new(env!("CARGO_BIN_EXE_armoury-tui"))
}

// ---------------------------------------------------------------------------
// --version
// ---------------------------------------------------------------------------

#[test]
fn test_version_exits_zero() {
    let out = armoury()
        .arg("--version")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--version should exit 0, got {:?}",
        out.status.code()
    );
}

#[test]
fn test_version_contains_name_and_semver() {
    let out = armoury()
        .arg("--version")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("armoury-tui"),
        "--version stdout should contain 'armoury-tui', got: {stdout}"
    );
    // Expect at least a major.minor.patch pattern somewhere in the output.
    let has_version = stdout
        .split_whitespace()
        .any(|w| {
            let parts: Vec<&str> = w.split('.').collect();
            parts.len() >= 2 && parts.iter().all(|p| p.parse::<u32>().is_ok())
        });
    assert!(
        has_version,
        "--version stdout should contain a version number (e.g. 1.0.0), got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// --help
// ---------------------------------------------------------------------------

#[test]
fn test_help_exits_zero() {
    let out = armoury()
        .arg("--help")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--help should exit 0, got {:?}",
        out.status.code()
    );
}

#[test]
fn test_help_contains_expected_keywords() {
    let out = armoury()
        .arg("--help")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for keyword in &["probe", "once", "json", "interval", "log", "replay"] {
        assert!(
            stdout.to_lowercase().contains(keyword),
            "--help stdout should mention '{keyword}', got:\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// --probe
// ---------------------------------------------------------------------------

#[test]
fn test_probe_exits_zero() {
    let out = armoury()
        .arg("--probe")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--probe should exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_probe_contains_header() {
    let out = armoury()
        .arg("--probe")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        stdout.contains("hardware probe"),
        "--probe should print 'hardware probe' header, got:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_probe_contains_expected_fields() {
    let out = armoury()
        .arg("--probe")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    for field in &["host", "product", "kernel", "cpu temp", "gpu", "fans", "battery"] {
        assert!(
            stdout.contains(field),
            "--probe output should contain '{field}', got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

// ---------------------------------------------------------------------------
// --once
// ---------------------------------------------------------------------------

#[test]
fn test_once_exits_zero() {
    let out = armoury()
        .arg("--once")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--once should exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_once_contains_expected_fields() {
    let out = armoury()
        .arg("--once")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    for field in &["profile", "cpu", "memory", "kbd light"] {
        assert!(
            stdout.contains(field),
            "--once output should contain '{field}', got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

// ---------------------------------------------------------------------------
// --json
// ---------------------------------------------------------------------------

#[test]
fn test_json_exits_zero() {
    let out = armoury()
        .arg("--json")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--json should exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_json_parses_and_has_required_keys() {
    let out = armoury()
        .arg("--json")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout should be valid JSON");
    assert!(val.is_object(), "--json should produce a JSON object");
    let obj = val.as_object().unwrap();
    for key in &[
        "profile",
        "cpu_load",
        "cpu_temp",
        "mem_pct",
        "battery_pct",
        "uptime_s",
    ] {
        assert!(
            obj.contains_key(*key),
            "--json output missing key '{key}', got keys: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_json_cpu_load_is_non_negative() {
    let out = armoury()
        .arg("--json")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let cpu_load = val["cpu_load"]
        .as_f64()
        .expect("cpu_load should be a number");
    assert!(
        cpu_load >= 0.0,
        "cpu_load should be >= 0, got {cpu_load}"
    );
}

#[test]
fn test_json_uptime_is_positive() {
    let out = armoury()
        .arg("--json")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let uptime = val["uptime_s"]
        .as_f64()
        .expect("uptime_s should be a number");
    assert!(uptime > 0.0, "uptime_s should be > 0, got {uptime}");
}

// ---------------------------------------------------------------------------
// --replay with fixture CSV
// ---------------------------------------------------------------------------

#[test]
fn test_replay_exits_zero() {
    let out = armoury()
        .arg("--replay")
        .arg("tests/fixtures/sample_run.csv")
        .output()
        .expect("failed to run binary");
    assert!(
        out.status.success(),
        "--replay should exit 0, got {:?}\nstderr: {}\nstdout: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_replay_header_and_sample_count() {
    let out = armoury()
        .arg("--replay")
        .arg("tests/fixtures/sample_run.csv")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        stdout.contains("replay"),
        "--replay output should contain 'replay', got:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stdout.contains("5 samples"),
        "--replay output should report '5 samples', got:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_replay_contains_metric_labels() {
    let out = armoury()
        .arg("--replay")
        .arg("tests/fixtures/sample_run.csv")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    for label in &["cpu load", "cpu temp", "gpu temp"] {
        assert!(
            stdout.contains(label),
            "--replay output should contain metric '{label}', got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn test_replay_contains_stat_keywords() {
    let out = armoury()
        .arg("--replay")
        .arg("tests/fixtures/sample_run.csv")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    for stat in &["min", "avg", "max"] {
        assert!(
            stdout.contains(stat),
            "--replay output should contain '{stat}', got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

// ---------------------------------------------------------------------------
// Invalid flag
// ---------------------------------------------------------------------------

#[test]
fn test_invalid_flag_exits_nonzero() {
    let out = armoury()
        .arg("--nonexistent-flag")
        .output()
        .expect("failed to run binary");
    assert!(
        !out.status.success(),
        "--nonexistent-flag should exit non-zero, got {:?}",
        out.status.code()
    );
}

// ---------------------------------------------------------------------------
// Conflicting flags (--probe and --once conflict per clap config)
// ---------------------------------------------------------------------------

#[test]
fn test_conflicting_flags_exits_nonzero() {
    let out = armoury()
        .arg("--probe")
        .arg("--once")
        .output()
        .expect("failed to run binary");
    assert!(
        !out.status.success(),
        "--probe --once should exit non-zero (conflicting flags), got {:?}",
        out.status.code()
    );
}
