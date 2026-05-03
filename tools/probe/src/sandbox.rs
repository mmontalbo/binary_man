//! Sandbox construction from setup commands.

use crate::parse::{FileContent, Property, SetupCommand};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Build sandbox state from setup commands.
/// `binary` is needed for invoke commands.
pub fn apply_setup(work_dir: &Path, binary: &str, commands: &[SetupCommand]) -> Result<()> {
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
                        fs::copy(src, &full)
                            .with_context(|| format!("copy {} -> {}", src, path))?;
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
                std::env::set_var(var, value);
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
                std::env::remove_var(var);
            }
            SetupCommand::Invoke { args } => {
                let output = std::process::Command::new(binary)
                    .args(args)
                    .current_dir(work_dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .env_clear()
                    .env("PATH", std::env::var("PATH").unwrap_or_default())
                    .env("HOME", work_dir)
                    .env("LANG", "C")
                    .env("LC_ALL", "C")
                    .output()
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
    Ok(())
}
