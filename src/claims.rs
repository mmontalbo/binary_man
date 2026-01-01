use crate::schema::{Claim, ClaimKind, ClaimSource, ClaimSourceType, ClaimStatus};
use std::collections::BTreeMap;

const EXTRACTOR_HELP_V0: &str = "parse:help:v0";
const CONFIDENCE_EXISTS: f32 = 0.9;
const CONFIDENCE_ARITY_REQUIRED: f32 = 0.7;
const CONFIDENCE_ARITY_OPTIONAL: f32 = 0.6;

#[derive(Debug, Clone)]
struct OptionToken {
    opt: String,
    arg: Option<ArgForm>,
}

#[derive(Debug, Clone)]
enum ArgForm {
    Required(String),
    Optional(String),
}

pub fn parse_help_text(source_path: &str, content: &str) -> Vec<Claim> {
    let mut claims_by_id: BTreeMap<String, Claim> = BTreeMap::new();
    let path_str = source_path.to_string();

    for (idx, line) in content.lines().enumerate() {
        let line_number = idx + 1;
        if !looks_like_option_table(line) {
            continue;
        }

        let spec = option_spec_segment(line);
        let tokens = parse_option_tokens(spec);
        if tokens.is_empty() {
            continue;
        }

        let canonical = choose_canonical(&tokens);
        let arg_form = select_arg_form(canonical, &tokens);

        let source = ClaimSource {
            source_type: ClaimSourceType::Help,
            path: path_str.clone(),
            line: Some(line_number as u64),
        };
        let raw_excerpt = line.to_string();

        let exists_id = format!("claim:option:opt={}:exists", canonical.opt);
        claims_by_id
            .entry(exists_id.clone())
            .or_insert_with(|| Claim {
                id: exists_id,
                text: format!("Option {} is listed in help output.", canonical.opt),
                kind: ClaimKind::Option,
                source: source.clone(),
                status: ClaimStatus::Unvalidated,
                extractor: EXTRACTOR_HELP_V0.to_string(),
                raw_excerpt: raw_excerpt.clone(),
                confidence: Some(CONFIDENCE_EXISTS),
            });

        if let Some(arg_form) = arg_form {
            let (form_text, confidence, qualifier, article) = match &arg_form {
                ArgForm::Required(arg) => (
                    format!("{}={}", canonical.opt, arg),
                    CONFIDENCE_ARITY_REQUIRED,
                    "required",
                    "a",
                ),
                ArgForm::Optional(arg) => (
                    format!("{}[={}]", canonical.opt, arg),
                    CONFIDENCE_ARITY_OPTIONAL,
                    "optional",
                    "an",
                ),
            };

            let arity_id = format!("claim:option:opt={}:arity", canonical.opt);
            claims_by_id
                .entry(arity_id.clone())
                .or_insert_with(|| Claim {
                    id: arity_id,
                    text: format!(
                        "Option {} accepts {} {} argument in `{}` form.",
                        canonical.opt, article, qualifier, form_text
                    ),
                    kind: ClaimKind::Option,
                    source: source.clone(),
                    status: ClaimStatus::Unvalidated,
                    extractor: EXTRACTOR_HELP_V0.to_string(),
                    raw_excerpt: raw_excerpt.clone(),
                    confidence: Some(confidence),
                });
        }
    }

    claims_by_id.into_values().collect()
}

fn looks_like_option_table(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if !trimmed.starts_with('-') {
        return false;
    }
    !trimmed.starts_with("---")
}

fn option_spec_segment(line: &str) -> &str {
    let trimmed = line.trim_end();
    let trimmed = trimmed.trim_start();
    split_on_double_space(trimmed).unwrap_or(trimmed)
}

fn split_on_double_space(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if is_whitespace(bytes[i]) && is_whitespace(bytes[i + 1]) {
            return Some(&line[..i]);
        }
    }
    None
}

fn is_whitespace(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

fn parse_option_tokens(spec: &str) -> Vec<OptionToken> {
    let mut tokens = Vec::new();

    for raw in spec.split_whitespace() {
        let token = strip_trailing_punct(raw);
        if let Some(parsed) = parse_option_token(token) {
            tokens.push(parsed);
        }
    }

    tokens
}

fn strip_trailing_punct(token: &str) -> &str {
    token.trim_end_matches(|c: char| matches!(c, ',' | ';' | ':'))
}

fn parse_option_token(token: &str) -> Option<OptionToken> {
    if let Some(parsed) = parse_long_option(token) {
        return Some(parsed);
    }
    parse_short_option(token)
}

fn parse_long_option(token: &str) -> Option<OptionToken> {
    if !token.starts_with("--") {
        return None;
    }
    if token.len() <= 2 {
        return None;
    }

    let (opt_part, arg_form) = split_arg_form(token)?;
    let name = &opt_part[2..];
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }

    Some(OptionToken {
        opt: opt_part.to_string(),
        arg: arg_form,
    })
}

fn parse_short_option(token: &str) -> Option<OptionToken> {
    if !token.starts_with('-') || token.starts_with("--") {
        return None;
    }
    if token.len() < 2 {
        return None;
    }

    let (opt_part, arg_form) = split_arg_form(token)?;
    let name = &opt_part[1..];
    if name.len() != 1 {
        return None;
    }
    let ch = name.chars().next()?;
    if !ch.is_ascii_alphanumeric() {
        return None;
    }

    Some(OptionToken {
        opt: opt_part.to_string(),
        arg: arg_form,
    })
}

fn split_arg_form(token: &str) -> Option<(&str, Option<ArgForm>)> {
    if let Some(idx) = token.find("[=") {
        if token.ends_with(']') {
            let opt_part = &token[..idx];
            let arg = &token[idx + 2..token.len() - 1];
            if arg.is_empty() {
                return None;
            }
            return Some((opt_part, Some(ArgForm::Optional(arg.to_string()))));
        }
    }

    if let Some(idx) = token.find('=') {
        let opt_part = &token[..idx];
        let arg = &token[idx + 1..];
        if arg.is_empty() {
            return None;
        }
        return Some((opt_part, Some(ArgForm::Required(arg.to_string()))));
    }

    Some((token, None))
}

fn choose_canonical(tokens: &[OptionToken]) -> &OptionToken {
    tokens
        .iter()
        .find(|t| t.opt.starts_with("--"))
        .unwrap_or(&tokens[0])
}

fn select_arg_form(canonical: &OptionToken, tokens: &[OptionToken]) -> Option<ArgForm> {
    if canonical.arg.is_some() {
        return canonical.arg.clone();
    }
    tokens.iter().find_map(|token| token.arg.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ClaimsFile;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    #[test]
    fn parses_help_text_surface_claims() {
        let path = Path::new("tests/data/ls_help.txt");
        let content = std::fs::read_to_string(path).expect("fixture missing");
        let source_path = path.display().to_string();
        let claims = parse_help_text(&source_path, &content);

        let ids: BTreeSet<String> = claims.iter().map(|c| c.id.clone()).collect();
        let expected: BTreeSet<String> = [
            "claim:option:opt=--all:exists",
            "claim:option:opt=--almost-all:exists",
            "claim:option:opt=--author:exists",
            "claim:option:opt=--block-size:arity",
            "claim:option:opt=--block-size:exists",
            "claim:option:opt=--color:arity",
            "claim:option:opt=--color:exists",
            "claim:option:opt=--dereference:exists",
            "claim:option:opt=--ignore-backups:exists",
            "claim:option:opt=--ignore:arity",
            "claim:option:opt=--ignore:exists",
            "claim:option:opt=--tabsize:arity",
            "claim:option:opt=--tabsize:exists",
            "claim:option:opt=-i:exists",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect();

        assert_eq!(ids, expected);

        for claim in &claims {
            assert_eq!(claim.extractor, EXTRACTOR_HELP_V0);
            assert!(matches!(&claim.status, ClaimStatus::Unvalidated));
            assert!(!claim.raw_excerpt.is_empty());
            assert_eq!(claim.source.path, source_path);
            assert!(claim.source.line.is_some());
        }
    }

    #[test]
    fn ignores_non_option_lines() {
        let content = "Examples:\n  ls --color=auto\n";
        let claims = parse_help_text("/tmp/help.txt", content);
        assert!(claims.is_empty());
    }

    #[test]
    fn uses_captured_source_path_label() {
        let content = "  -a, --all  include dotfiles\n";
        let claims = parse_help_text("<captured:--help>", content);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].source.path, "<captured:--help>");
    }

    #[test]
    fn golden_ls_claims_snapshot() {
        let path = Path::new("tests/data/ls_help.txt");
        let content = fs::read_to_string(path).expect("fixture missing");
        let source_path = path.display().to_string();
        let claims = parse_help_text(&source_path, &content);
        let claims_file = ClaimsFile {
            binary_identity: None,
            invocation: None,
            capture_error: None,
            claims,
        };

        let actual = serde_json::to_string_pretty(&claims_file).expect("serialize claims");
        let expected = fs::read_to_string("tests/golden/ls_claims.json").expect("golden missing");
        assert_eq!(expected.trim_end(), actual);
    }
}
