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
}

impl Sandbox {
    /// Find bwrap or fail with a clear error.
    pub fn new() -> Result<Self> {
        let bwrap = which::which("bwrap")
            .context("bwrap not found — install bubblewrap for sandbox isolation")?;
        Ok(Sandbox { bwrap })
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
    ) -> Command {
        let mut cmd = self.bwrap_base(work_dir, "/workspace", env_vars);
        cmd.arg("--");
        cmd.arg(binary);
        for arg in args {
            cmd.arg(arg);
        }

        cmd
    }

    /// Build a bwrap Command that runs a shell script with a batch directory mounted.
    pub fn batch_command(
        &self,
        batch_dir: &Path,
        script_name: &str,
        env_vars: &HashMap<String, String>,
    ) -> Command {
        let mut cmd = self.bwrap_base(batch_dir, "/batch", env_vars);
        cmd.arg("--");
        cmd.arg("sh").arg(format!("/batch/{}", script_name));
        cmd
    }

    /// Common bwrap setup: namespace isolation, system paths, workspace bind, env.
    fn bwrap_base(&self, work_dir: &Path, mount_point: &str, env_vars: &HashMap<String, String>) -> Command {
        let mut cmd = Command::new(&self.bwrap);
        cmd.arg("--unshare-net").arg("--die-with-parent");
        for path in &["/nix", "/usr", "/bin", "/lib", "/lib64", "/etc", "/run"] {
            if Path::new(path).exists() {
                cmd.arg("--ro-bind").arg(path).arg(path);
            }
        }
        cmd.arg("--proc").arg("/proc");
        cmd.arg("--dev").arg("/dev");
        cmd.arg("--tmpfs").arg("/tmp");
        cmd.arg("--bind").arg(work_dir).arg(mount_point);
        cmd.arg("--chdir").arg(mount_point);
        cmd.arg("--setenv").arg("HOME").arg(mount_point);
        cmd.arg("--setenv").arg("PATH").arg(std::env::var("PATH").unwrap_or_default());
        cmd.arg("--setenv").arg("LANG").arg("C");
        cmd.arg("--setenv").arg("LC_ALL").arg("C");
        for (k, v) in env_vars {
            cmd.arg("--setenv").arg(k).arg(v);
        }
        cmd
    }
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
                let mut invoke = sandbox.command(binary, &str_args, work_dir, &env_vars);
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
