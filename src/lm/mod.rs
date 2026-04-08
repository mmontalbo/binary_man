//! LM plugin system for bman verification.
//!
//! Provides a trait-based abstraction over different LM backends:
//! - `ClaudeCodePlugin`: Native Claude CLI integration with persistent process
//! - `CommandPlugin`: External command (any stdin→stdout LM, or BMAN_LM_COMMAND)

mod claude_code;
mod command;
mod ollama;

pub use claude_code::ClaudeCodePlugin;
pub use command::CommandPlugin;
pub use ollama::OllamaPlugin;

use anyhow::Result;
use std::time::Duration;

/// Configure a child process to die when its parent exits.
///
/// On Linux this uses `prctl(PR_SET_PDEATHSIG, SIGKILL)` — a kernel
/// guarantee that survives even if the parent is SIGKILLed.
///
/// On macOS there is no equivalent kernel primitive. The parent process
/// handles cleanup via `Drop` impls, which covers normal exits but not
/// `SIGKILL` of the parent. This is an accepted limitation.
///
/// Called inside `pre_exec` closures to prevent orphaned subprocesses.
#[cfg(target_os = "linux")]
pub(crate) fn set_die_with_parent() {
    unsafe {
        libc::prctl(1, 9); // PR_SET_PDEATHSIG = 1, SIGKILL = 9
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn set_die_with_parent() {
    // No equivalent on macOS; parent cleanup is handled by Drop impls.
}

/// A language model plugin for bman verification.
///
/// Plugins manage the lifecycle of LM connections and provide prompt/response
/// functionality with timeout support.
pub trait LmPlugin: Send {
    /// Initialize the plugin (spawn processes, establish connections, etc).
    fn init(&mut self) -> Result<()>;

    /// Send a prompt and get a response with timeout.
    fn prompt(&mut self, prompt: &str, timeout: Duration) -> Result<String>;

    /// Reset the session (restart underlying process on errors).
    fn reset(&mut self) -> Result<()>;

    /// Clean shutdown.
    fn shutdown(&mut self) -> Result<()>;

    /// Whether this plugin maintains conversation state across prompts.
    /// Stateful plugins accumulate context, so incremental prompts can be used.
    /// Stateless plugins (command) need full context each time.
    fn is_stateful(&self) -> bool;
}

/// Plugin configuration parsed from CLI arguments.
#[derive(Debug, Clone)]
pub enum LmConfig {
    /// Native Claude Code integration: "claude:haiku", "claude:sonnet"
    Claude { model: String },
    /// Ollama local server: "ollama:model-tag"
    Ollama { model: String },
    /// External command (any stdin→stdout LM wrapper)
    Command { cmd: String },
}

/// Parse --lm argument into config.
///
/// Format: "claude:model" for native, or command string for external backends.
///
/// # Examples
/// - `"claude:haiku"` -> `LmConfig::Claude { model: "haiku" }`
/// - `"claude:sonnet"` -> `LmConfig::Claude { model: "sonnet" }`
/// - `"my-lm-script"` -> `LmConfig::Command { cmd: "my-lm-script" }`
/// - `"/path/to/lm"` -> `LmConfig::Command { cmd: "/path/to/lm" }`
pub fn parse_lm_arg(arg: &str, env_fallback: Option<&str>) -> LmConfig {
    // Check for native Claude plugin syntax
    if let Some(model) = arg.strip_prefix("claude:") {
        return LmConfig::Claude {
            model: model.to_string(),
        };
    }

    // Check for Ollama plugin syntax
    if let Some(model) = arg.strip_prefix("ollama:") {
        return LmConfig::Ollama {
            model: model.to_string(),
        };
    }

    // Looks like a command (has space, starts with path)
    if arg.contains(' ') || arg.starts_with('/') || arg.starts_with('.') {
        return LmConfig::Command {
            cmd: arg.to_string(),
        };
    }

    // If arg doesn't match native format, check environment fallback
    if let Some(env_cmd) = env_fallback {
        return LmConfig::Command {
            cmd: env_cmd.to_string(),
        };
    }

    // Default to Claude haiku
    LmConfig::Claude {
        model: "haiku".to_string(),
    }
}

/// Create a plugin instance from config.
pub fn create_plugin(config: &LmConfig) -> Box<dyn LmPlugin> {
    match config {
        LmConfig::Claude { model } => Box::new(ClaudeCodePlugin::new(model)),
        LmConfig::Ollama { model } => Box::new(OllamaPlugin::new(model)),
        LmConfig::Command { cmd } => Box::new(CommandPlugin::new(cmd)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_claude_haiku() {
        let config = parse_lm_arg("claude:haiku", None);
        match config {
            LmConfig::Claude { model } => assert_eq!(model, "haiku"),
            _ => panic!("Expected Claude config"),
        }
    }

    #[test]
    fn test_parse_claude_sonnet() {
        let config = parse_lm_arg("claude:sonnet", None);
        match config {
            LmConfig::Claude { model } => assert_eq!(model, "sonnet"),
            _ => panic!("Expected Claude config"),
        }
    }

    #[test]
    fn test_parse_command_with_space() {
        let config = parse_lm_arg("my-lm -p --model haiku", None);
        match config {
            LmConfig::Command { cmd } => assert_eq!(cmd, "my-lm -p --model haiku"),
            _ => panic!("Expected Command config"),
        }
    }

    #[test]
    fn test_parse_command_absolute_path() {
        let config = parse_lm_arg("/usr/bin/my-lm", None);
        match config {
            LmConfig::Command { cmd } => assert_eq!(cmd, "/usr/bin/my-lm"),
            _ => panic!("Expected Command config"),
        }
    }

    #[test]
    fn test_parse_command_relative_path() {
        let config = parse_lm_arg("./my-lm", None);
        match config {
            LmConfig::Command { cmd } => assert_eq!(cmd, "./my-lm"),
            _ => panic!("Expected Command config"),
        }
    }

    #[test]
    fn test_parse_env_fallback() {
        let config = parse_lm_arg("unknown", Some("env-command arg"));
        match config {
            LmConfig::Command { cmd } => assert_eq!(cmd, "env-command arg"),
            _ => panic!("Expected Command config from env"),
        }
    }

    #[test]
    fn test_parse_default_claude() {
        let config = parse_lm_arg("unknown", None);
        match config {
            LmConfig::Claude { model } => assert_eq!(model, "haiku"),
            _ => panic!("Expected default Claude config"),
        }
    }
}
