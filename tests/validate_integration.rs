use std::path::{Path, PathBuf};
use std::process::Command;

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn ls_help_available(ls_path: &Path) -> bool {
    ["--help", "-h"].iter().any(|arg| {
        let output = match Command::new(ls_path)
            .arg(arg)
            .env_clear()
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .output()
        {
            Ok(output) => output,
            Err(_) => return false,
        };
        !output.stdout.is_empty() || !output.stderr.is_empty()
    })
}

#[test]
fn validates_ls_all_option_when_help_is_available() {
    let Some(ls_path) = find_in_path("ls") else {
        return;
    };
    if !ls_help_available(&ls_path) {
        return;
    }

    let bin = env!("CARGO_BIN_EXE_binary-man");
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let claims_path = temp_dir.path().join("claims.json");
    let out_path = temp_dir.path().join("validation.json");

    let claims_status = Command::new(bin)
        .arg("claims")
        .arg("--binary")
        .arg(&ls_path)
        .arg("--out")
        .arg(&claims_path)
        .status()
        .expect("run claims");
    assert!(claims_status.success());

    let validate_status = Command::new(bin)
        .arg("validate")
        .arg("--binary")
        .arg(&ls_path)
        .arg("--claims")
        .arg(&claims_path)
        .arg("--out")
        .arg(&out_path)
        .status()
        .expect("run validate");
    assert!(validate_status.success());

    let content = std::fs::read_to_string(&out_path).expect("read validation report");
    let report: serde_json::Value =
        serde_json::from_str(&content).expect("parse validation report");
    let results = report
        .get("results")
        .and_then(|value| value.as_array())
        .expect("results array");

    let result = results
        .iter()
        .find(|value| {
            value.get("claim_id").and_then(|value| value.as_str())
                == Some("claim:option:opt=--all:exists")
        })
        .expect("expected --all result");
    let status = result
        .get("status")
        .and_then(|value| value.as_str())
        .expect("status string");

    assert_eq!(status, "confirmed");

    let has_binding = results.iter().any(|value| {
        value.get("claim_id").and_then(|value| value.as_str())
            == Some("claim:option:opt=--block-size:binding")
    });
    assert!(has_binding, "expected --block-size binding result");
}

#[test]
fn ls_hide_requires_argument_when_help_is_available() {
    let Some(ls_path) = find_in_path("ls") else {
        return;
    };
    if !ls_help_available(&ls_path) {
        return;
    }

    let output = match Command::new(&ls_path)
        .arg("--hide")
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .output()
    {
        Ok(output) => output,
        Err(_) => return,
    };

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires an argument"));
}
