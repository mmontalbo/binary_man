//! Miscellaneous utilities shared across the workflow.
//!
//! These helpers keep path handling, truncation, and hashing consistent so
//! the core logic can remain focused on workflow decisions.
use sha2::Digest;
use std::path::Path;

/// Render a path relative to a base when possible.
///
/// Relative paths make status output more readable and stable across machines.
pub fn display_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if let Ok(relative) = path.strip_prefix(base) {
            return relative.display().to_string();
        }
    }
    path.display().to_string()
}

/// Truncate bytes to a safe UTF-8 string preview.
///
/// This keeps previews bounded without breaking multi-byte characters.
pub fn truncate_bytes(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_string(&text, max_bytes)
}

/// Truncate a string to a maximum byte count without breaking UTF-8.
pub fn truncate_string(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        if truncated.len() + ch.len_utf8() > max_bytes {
            break;
        }
        truncated.push(ch);
    }
    truncated
}

/// Hash bytes to a lowercase hex SHA-256 digest for evidence tracking.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
