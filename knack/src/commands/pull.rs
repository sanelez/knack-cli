//! `knack pull <slug>[@<semver>] [--target ./dir]` — write a skill to disk.
//!
//! Layout (matches spec § III):
//!
//! ```text
//! <target>/<slug>/
//!   SKILL.md
//!   intuition.md
//!   meta.knack.yaml
//! ```
//!
//! Idempotent: re-pulling overwrites only when content actually changed.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{ApiClient, skills as api_skills};
use crate::errors::{CliError, CliResult};
use crate::output::{OutputMode, chatter, emit_err, emit_ok};

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Skill identifier — `<slug>` or `<slug>@<semver>`.
    pub slug_at_version: String,

    /// Output directory (default `~/.knack/skills/`). The skill is written
    /// into a `<slug>/` subdirectory underneath.
    #[arg(long)]
    pub target: Option<PathBuf>,
}

pub async fn run(args: PullArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let (slug, version_filter) = parse_slug_at_version(&args.slug_at_version);

    let skill = match api_skills::find_by_slug(&client, slug).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            let err = CliError::NotFound(format!("skill `{slug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let semver = match version_filter {
        Some(v) => v.to_string(),
        None => match skill.current_version_semver.clone() {
            Some(v) => v,
            None => {
                let err = CliError::NotFound(format!("skill `{slug}` has no published version"));
                emit_err(mode, &err);
                return Err(err);
            }
        },
    };

    let version = match api_skills::get_version(&client, &skill.id, &semver).await {
        Ok(v) => v,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let target_root = args.target.unwrap_or_else(|| client.config.skills_dir.clone());
    let dir = target_root.join(&skill.slug);
    std::fs::create_dir_all(&dir)?;

    let mut written = Vec::new();
    written.extend(write_if_changed(&dir.join("SKILL.md"), &version.skill_md)?);
    written.extend(write_if_changed(&dir.join("intuition.md"), &version.intuition_md)?);
    written.extend(write_if_changed(&dir.join("meta.knack.yaml"), &version.meta_yaml)?);

    chatter(mode, format!("pulled {}@{} → {}", skill.slug, version.version, dir.display()));

    emit_ok(
        mode,
        json!({
            "skill_id": skill.id,
            "slug": skill.slug,
            "version": version.version,
            "path": dir,
            "files_written": written,
        }),
        || {
            println!("✓ {}@{} → {}", skill.slug, version.version, dir.display());
        },
    );
    Ok(())
}

/// Splits `monthly-close@1.0` into `("monthly-close", Some("1.0"))`. A bare
/// slug returns `(slug, None)`.
fn parse_slug_at_version(s: &str) -> (&str, Option<&str>) {
    match s.split_once('@') {
        Some((slug, ver)) => (slug, Some(ver)),
        None => (s, None),
    }
}

/// Idempotent write — returns the path iff bytes changed (or file was new).
fn write_if_changed(path: &std::path::Path, content: &str) -> std::io::Result<Vec<PathBuf>> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing == content {
            return Ok(Vec::new());
        }
    }
    std::fs::write(path, content)?;
    Ok(vec![path.to_path_buf()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_version() {
        assert_eq!(parse_slug_at_version("foo@1.0.0"), ("foo", Some("1.0.0")));
    }

    #[test]
    fn parse_without_version() {
        assert_eq!(parse_slug_at_version("foo"), ("foo", None));
    }

    #[test]
    fn parse_v_prefix_is_left_intact_for_normalize() {
        // Server normalizes `v1.0` → `1.0.0`. CLI doesn't pre-strip.
        assert_eq!(parse_slug_at_version("foo@v1.0"), ("foo", Some("v1.0")));
    }

    #[test]
    fn write_if_changed_skips_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let written = write_if_changed(&p, "hello").unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn write_if_changed_writes_on_diff() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let written = write_if_changed(&p, "world").unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "world");
    }

    #[test]
    fn write_if_changed_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.md");
        let written = write_if_changed(&p, "data").unwrap();
        assert_eq!(written.len(), 1);
        assert!(p.exists());
    }
}
