//! Self-host workspace config reader (`<repo>/knack.yaml`).
//!
//! `bootstrap_repo` seeds this file with `owner`, `repo`, and `format`. Users
//! can add `auto_push: false` to opt every telemetry event in this workspace
//! out of the synchronous `git push origin main` that follows the commit.
//! CLI `--no-push` and `KNACK_AUTO_PUSH=0` cover the per-invocation and
//! per-shell case; this file covers per-workspace.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct WorkspaceConfig {
    #[serde(default)]
    auto_push: Option<bool>,
}

/// Return the workspace's `auto_push` setting from `<repo>/knack.yaml`.
///
/// `Ok(Some(true))` / `Ok(Some(false))` if explicitly set. `Ok(None)` if the
/// file or field is missing — caller picks the default. Errors only on a
/// genuinely malformed YAML file (so a stray typo doesn't silently flip the
/// telemetry-push behavior).
pub fn read_workspace_auto_push(repo: &Path) -> Result<Option<bool>> {
    let path = repo.join("knack.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let cfg: WorkspaceConfig = serde_yaml::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg.auto_push)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn missing_file_returns_none() {
        let dir = tempdir().unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), None);
    }

    #[test]
    fn yaml_without_auto_push_returns_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("knack.yaml"), "owner: someone\nrepo: x\n").unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), None);
    }

    #[test]
    fn yaml_with_auto_push_false_returns_false() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("knack.yaml"),
            "owner: someone\nrepo: x\nauto_push: false\n",
        )
        .unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), Some(false));
    }

    #[test]
    fn yaml_with_auto_push_true_returns_true() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("knack.yaml"),
            "owner: someone\nrepo: x\nauto_push: true\n",
        )
        .unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), Some(true));
    }
}
