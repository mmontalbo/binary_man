use crate::enrich;
use crate::pack;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

pub(crate) struct EnrichContext {
    pub(crate) paths: enrich::DocPackPaths,
    pub(crate) manifest: Option<pack::PackManifest>,
    pub(crate) binary_name: Option<String>,
    pub(crate) config: enrich::EnrichConfig,
    pub(crate) config_exists: bool,
    pub(crate) lock: Option<enrich::EnrichLock>,
    pub(crate) lock_status: enrich::LockStatus,
    pub(crate) plan: Option<enrich::EnrichPlan>,
}

impl EnrichContext {
    pub(crate) fn load(doc_pack_root: PathBuf) -> Result<Self> {
        let paths = enrich::DocPackPaths::new(doc_pack_root);
        let manifest = load_manifest_optional(&paths)?;
        let binary_name = manifest.as_ref().map(|m| m.binary_name.clone());

        let config_exists = paths.config_path().is_file();
        let config = if config_exists {
            enrich::load_config(paths.root())?
        } else {
            enrich::default_config()
        };

        let lock = paths
            .lock_path()
            .is_file()
            .then(|| enrich::load_lock(paths.root()))
            .transpose()?;
        let lock_status = enrich::lock_status(paths.root(), lock.as_ref())?;

        let plan = paths
            .plan_path()
            .is_file()
            .then(|| crate::status::load_plan(paths.root()))
            .transpose()?;

        Ok(Self {
            paths,
            manifest,
            binary_name,
            config,
            config_exists,
            lock,
            lock_status,
            plan,
        })
    }

    pub(crate) fn binary_name(&self) -> Option<&str> {
        self.binary_name.as_deref()
    }

    pub(crate) fn require_config(&self) -> Result<()> {
        if self.config_exists {
            return Ok(());
        }
        Err(anyhow!(
            "missing enrich config at {} (run `bman init --doc-pack {}` first)",
            self.paths.config_path().display(),
            self.paths.root().display()
        ))
    }

    pub(crate) fn lock_for_plan(
        &self,
        force: bool,
    ) -> Result<(enrich::EnrichLock, enrich::LockStatus, bool)> {
        let mut force_used = false;
        let lock = match self.lock.as_ref() {
            Some(lock) => lock.clone(),
            None => {
                if !force {
                    return Err(anyhow!(
                        "missing lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                        self.paths.lock_path().display(),
                        self.paths.root().display()
                    ));
                }
                force_used = true;
                enrich::build_lock(self.paths.root(), &self.config, self.binary_name())?
            }
        };

        let lock_status = enrich::lock_status(self.paths.root(), Some(&lock))?;
        if lock_status.stale && !force {
            return Err(anyhow!(
                "stale lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                self.paths.lock_path().display(),
                self.paths.root().display()
            ));
        }
        if lock_status.stale {
            force_used = true;
        }
        Ok((lock, lock_status, force_used))
    }

    pub(crate) fn lock_for_apply(
        &self,
        force: bool,
    ) -> Result<(Option<enrich::EnrichLock>, enrich::LockStatus, bool)> {
        let lock_status = self.lock_status.clone();
        let force_used = force && (!lock_status.present || lock_status.stale);
        if (!lock_status.present || lock_status.stale) && !force {
            return Err(anyhow!(
                "missing or stale lock at {} (run `bman validate --doc-pack {}` or pass --force)",
                self.paths.lock_path().display(),
                self.paths.root().display()
            ));
        }
        Ok((self.lock.clone(), lock_status, force_used))
    }

    pub(crate) fn require_plan(&self) -> Result<enrich::EnrichPlan> {
        self.plan.clone().ok_or_else(|| {
            anyhow!(
                "missing plan at {} (run `bman plan --doc-pack {}` first)",
                self.paths.plan_path().display(),
                self.paths.root().display()
            )
        })
    }
}

pub(crate) fn load_manifest_optional(
    paths: &enrich::DocPackPaths,
) -> Result<Option<pack::PackManifest>> {
    let pack_root = paths.pack_root();
    let manifest_path = paths.pack_manifest_path();
    if !manifest_path.is_file() {
        return Ok(None);
    }
    Ok(Some(pack::load_manifest(&pack_root)?))
}
