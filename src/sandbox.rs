//! Sandbox construction and execution via bubblewrap.

use crate::parse::{FileContent, Property, SetupCommand};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Path to the bwrap binary. Found once at startup.
pub struct Sandbox {
    bwrap: PathBuf,
    pub strace: Option<PathBuf>,
}

impl Sandbox {
    /// Find bwrap or fail with a clear error.
    pub fn new(trace: bool) -> Result<Self> {
        let bwrap = which::which("bwrap")
            .context("bwrap not found — install bubblewrap for sandbox isolation")?;
        let strace = if trace {
            Some(which::which("strace")
                .context("strace not found — install strace for --trace mode")?)
        } else {
            None
        };
        Ok(Sandbox { bwrap, strace })
    }

    /// Build a Command that runs `binary args...` inside the bwrap sandbox.
    /// The workspace is bind-mounted read-write at /workspace.
    /// If trace_dir is provided, it's bind-mounted at /trace for strace output.
    pub fn command(
        &self,
        binary: &str,
        args: &[&str],
        work_dir: &Path,
        env_vars: &HashMap<String, String>,
        trace_dir: Option<&Path>,
    ) -> Command {
        let mut cmd = Command::new(&self.bwrap);

        // Namespace isolation
        cmd.arg("--unshare-net");
        cmd.arg("--die-with-parent");

        // Read-only system paths (only bind what exists)
        for path in &["/nix", "/usr", "/bin", "/lib", "/lib64", "/etc", "/run"] {
            if Path::new(path).exists() {
                cmd.arg("--ro-bind").arg(path).arg(path);
            }
        }

        // Proc and dev
        cmd.arg("--proc").arg("/proc");
        cmd.arg("--dev").arg("/dev");
        cmd.arg("--tmpfs").arg("/tmp");

        // Read-write workspace
        cmd.arg("--bind").arg(work_dir).arg("/workspace");
        cmd.arg("--chdir").arg("/workspace");

        // Trace output directory (separate from workspace)
        if let Some(td) = trace_dir {
            cmd.arg("--bind").arg(td).arg("/trace");
        }

        // Environment
        cmd.arg("--setenv").arg("HOME").arg("/workspace");
        cmd.arg("--setenv").arg("PATH").arg(std::env::var("PATH").unwrap_or_default());
        cmd.arg("--setenv").arg("LANG").arg("C");
        cmd.arg("--setenv").arg("LC_ALL").arg("C");
        for (k, v) in env_vars {
            cmd.arg("--setenv").arg(k).arg(v);
        }

        // The actual command — optionally wrapped in strace
        cmd.arg("--");
        if let Some(ref strace) = self.strace {
            cmd.arg(strace);
            cmd.arg("-f");
            cmd.arg("-e").arg("trace=openat");
            cmd.arg("-o").arg("/trace/.bgrid-trace");
            cmd.arg("-qq");
            cmd.arg("--");
        }
        cmd.arg(binary);
        for arg in args {
            cmd.arg(arg);
        }

        cmd
    }
}

/// Parse strace output for workspace file access patterns.
/// Returns (files_read, files_failed) — paths relative to workspace.
pub fn parse_trace(trace_dir: &Path) -> (Vec<String>, Vec<String>) {
    let trace_path = trace_dir.join(".bgrid-trace");
    let mut reads = Vec::new();
    let mut failed = Vec::new();
    let mut seen_reads = std::collections::HashSet::new();
    let mut seen_failed = std::collections::HashSet::new();

    if let Ok(content) = std::fs::read_to_string(&trace_path) {
        for line in content.lines() {
            // Match: openat(AT_FDCWD, "path", flags) = result
            let Some(start) = line.find('"') else { continue };
            let Some(end) = line[start + 1..].find('"') else { continue };
            let path = &line[start + 1..start + 1 + end];

            // Skip non-workspace paths
            if path.starts_with("/nix/") || path.starts_with("/etc/")
                || path.starts_with("/proc/") || path.starts_with("/dev/")
                || path.starts_with("/usr/") || path.starts_with("/lib")
                || path.starts_with("/run/") || path.starts_with("/tmp/")
                || path == "/workspace/.bgrid-trace"
            {
                continue;
            }

            // Normalize path: strip /workspace/ prefix or ./ prefix
            let rel = path.strip_prefix("/workspace/")
                .or_else(|| path.strip_prefix("./"))
                .unwrap_or(path);

            if rel.is_empty() || rel == "." { continue; }

            if line.contains("ENOENT") || line.contains("EACCES") {
                if seen_failed.insert(rel.to_string()) {
                    failed.push(rel.to_string());
                }
            } else if line.contains("= ") && !line.contains("= -1")
                && seen_reads.insert(rel.to_string()) {
                    reads.push(rel.to_string());
            }
        }
        // Clean up trace file
        let _ = std::fs::remove_file(&trace_path);
    }

    reads.sort();
    failed.sort();
    (reads, failed)
}

/// Build sandbox state from setup commands.
/// Returns accumulated env vars for use by run invocations.
pub fn apply_setup(
    work_dir: &Path,
    binary: &str,
    commands: &[SetupCommand],
    probe_dir: &Path,
    sandbox: &Sandbox,
) -> Result<HashMap<String, String>> {
    let mut env_vars: HashMap<String, String> = HashMap::new();

    for cmd in commands {
        match cmd {
            SetupCommand::CreateFile { path, content } => {
                let full = work_dir.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dirs for {}", path))?;
                }
                match content {
                    FileContent::Lines(lines) => {
                        let text = lines.join("\n") + "\n";
                        fs::write(&full, &text)
                            .with_context(|| format!("write {}", path))?;
                    }
                    FileContent::Size(n) => {
                        let data = "x".repeat(*n);
                        fs::write(&full, &data)
                            .with_context(|| format!("write {} ({} bytes)", path, n))?;
                    }
                    FileContent::Empty => {
                        fs::write(&full, "")
                            .with_context(|| format!("write empty {}", path))?;
                    }
                    FileContent::From(src) => {
                        let resolved = if Path::new(src).is_absolute() {
                            PathBuf::from(src)
                        } else {
                            probe_dir.join(src)
                        };
                        fs::copy(&resolved, &full)
                            .with_context(|| format!("copy {} -> {}", resolved.display(), path))?;
                    }
                }
            }
            SetupCommand::CreateDir { path } => {
                let full = work_dir.join(path);
                fs::create_dir_all(&full)
                    .with_context(|| format!("create dir {}", path))?;
            }
            SetupCommand::CreateLink { path, target } => {
                let full = work_dir.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)?;
                }
                let _ = fs::remove_file(&full); // idempotent: allow overwrite by vary
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &full)
                    .with_context(|| format!("symlink {} -> {}", path, target))?;
            }
            SetupCommand::SetProps { path, props } => {
                let full = work_dir.join(path);
                for prop in props {
                    match prop {
                        Property::Executable => {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let meta = fs::metadata(&full)
                                    .with_context(|| format!("stat {}", path))?;
                                let mut perms = meta.permissions();
                                perms.set_mode(perms.mode() | 0o111);
                                fs::set_permissions(&full, perms)?;
                            }
                        }
                        Property::ReadOnly => {
                            let meta = fs::metadata(&full)?;
                            let mut perms = meta.permissions();
                            perms.set_readonly(true);
                            fs::set_permissions(&full, perms)?;
                        }
                        Property::MtimeOld => {
                            #[cfg(unix)]
                            {
                                let old_time = 946684800; // 2000-01-01
                                let times = [
                                    libc::timespec { tv_sec: old_time, tv_nsec: 0 },
                                    libc::timespec { tv_sec: old_time, tv_nsec: 0 },
                                ];
                                let path_c = std::ffi::CString::new(
                                    full.to_string_lossy().as_bytes(),
                                ).unwrap();
                                unsafe {
                                    libc::utimensat(
                                        libc::AT_FDCWD,
                                        path_c.as_ptr(),
                                        times.as_ptr(),
                                        0,
                                    );
                                }
                            }
                        }
                        Property::MtimeRecent => {
                            if full.exists() {
                                let content = fs::read(&full).unwrap_or_default();
                                fs::write(&full, &content)?;
                            }
                        }
                    }
                }
            }
            SetupCommand::SetEnv { var, value } => {
                env_vars.insert(var.clone(), value.clone());
            }
            SetupCommand::Remove { path } => {
                let full = work_dir.join(path);
                if full.is_dir() {
                    let _ = fs::remove_dir_all(&full);
                } else {
                    let _ = fs::remove_file(&full);
                }
            }
            SetupCommand::RemoveEnv { var } => {
                env_vars.remove(var.as_str());
            }
            SetupCommand::Invoke { args } => {
                let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let mut invoke = sandbox.command(binary, &str_args, work_dir, &env_vars, None);
                invoke
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped());

                let output = invoke.output()
                    .with_context(|| format!("invoke {:?}", args))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!(
                        "invoke {:?} failed (exit {}): {}",
                        args,
                        output.status.code().unwrap_or(-1),
                        stderr.trim()
                    );
                }
            }
        }
    }
    Ok(env_vars)
}
