use anyhow::{anyhow, Context, Result};
use sha2::Digest;
use std::env;
use std::path::{Path, PathBuf};

pub fn resolve_flake_ref(input: &str) -> Result<String> {
    let (path_part, attr_part) = match input.split_once('#') {
        Some((path_part, attr_part)) => (path_part, Some(attr_part)),
        None => (input, None),
    };

    if path_part.is_empty() {
        return Ok(input.to_string());
    }

    let path = Path::new(path_part);
    let should_resolve = path_part.starts_with('.') || path.is_absolute() || path.exists();
    if !should_resolve {
        return Ok(input.to_string());
    }

    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd_candidate = env::current_dir().ok().map(|cwd| cwd.join(path));
        if let Some(candidate) = cwd_candidate.as_ref().filter(|p| p.exists()) {
            candidate.clone()
        } else {
            let manifest_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
            if manifest_candidate.exists() {
                manifest_candidate
            } else {
                cwd_candidate.unwrap_or_else(|| path.to_path_buf())
            }
        }
    };

    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("resolve lens flake path {}", candidate.display()))?;
    let resolved_str = resolved
        .to_str()
        .ok_or_else(|| anyhow!("lens flake path is not valid UTF-8"))?;

    Ok(match attr_part {
        Some(attr) if !attr.is_empty() => format!("{resolved_str}#{attr}"),
        _ => resolved_str.to_string(),
    })
}

pub fn display_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if let Ok(relative) = path.strip_prefix(base) {
            return relative.display().to_string();
        }
    }
    path.display().to_string()
}

pub fn truncate_bytes(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_string(&text, max_bytes)
}

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

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
