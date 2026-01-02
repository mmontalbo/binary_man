//! Schema types for claims, evidence, and reports.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryIdentity {
    pub path: PathBuf,
    pub hash: Hash,
    pub platform: Platform,
    pub env: EnvSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hash {
    pub algo: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvSnapshot {
    pub locale: String,
    pub tz: String,
    pub term: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimsFile {
    pub binary_identity: Option<BinaryIdentity>,
    pub invocation: Option<String>,
    pub capture_error: Option<String>,
    pub claims: Vec<Claim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub text: String,
    pub kind: ClaimKind,
    pub source: ClaimSource,
    pub status: ClaimStatus,
    pub extractor: String,
    pub raw_excerpt: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Option,
    Behavior,
    Env,
    Io,
    Error,
    ExitStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimSource {
    #[serde(rename = "type")]
    pub source_type: ClaimSourceType,
    pub path: String,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSourceType {
    Man,
    Help,
    Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Unvalidated,
    Confirmed,
    Refuted,
    Undetermined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub claim_id: String,
    pub status: ValidationStatus,
    pub method: ValidationMethod,
    pub determinism: Option<Determinism>,
    pub attempts: Vec<Evidence>,
    pub observed: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Confirmed,
    Refuted,
    Undetermined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMethod {
    AcceptanceTest,
    BehaviorFixture,
    StderrMatch,
    ExitCode,
    OutputDiff,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Determinism {
    Deterministic,
    EnvSensitive,
    Flaky,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub binary_identity: BinaryIdentity,
    pub results: Vec<ValidationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegenerationReport {
    pub binary_identity: BinaryIdentity,
    pub claims_path: PathBuf,
    pub results_path: PathBuf,
    pub out_man: PathBuf,
}

/// Compute binary identity using a provided environment snapshot.
pub fn compute_binary_identity_with_env(path: &Path, env: EnvSnapshot) -> Result<BinaryIdentity> {
    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let bytes = std::fs::read(&abs_path)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();

    Ok(BinaryIdentity {
        path: abs_path,
        hash: Hash {
            algo: "blake3".to_string(),
            value: hash,
        },
        platform: Platform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        env,
    })
}
