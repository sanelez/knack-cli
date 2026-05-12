//! `knack pull <slug>[@<semver>] [--target ./dir]` — write a skill to disk.
//!
//! V2a default: if the SkillVersion has a `packed_s3_key`, download the
//! tarball via the bundle endpoint and unpack the full Anthropic Agent
//! Skills folder (SKILL.md + meta.knack.yaml + intuition.md + scripts/ +
//! assets/ + references/ + examples/ + tests/) into the target directory.
//!
//! Legacy fallback: pre-V2a versions store only three text columns and have
//! no bundle. We write just `SKILL.md`, `intuition.md`, `meta.knack.yaml`
//! into the target — matching the original layout.
//!
//! Idempotent on the legacy path: re-pulling overwrites only when content
//! actually changed.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};
use crate::skill_pack::unpack_skill;

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

    let target_root = args
        .target
        .unwrap_or_else(|| client.config.skills_dir.clone());
    let dir = target_root.join(&skill.slug);
    std::fs::create_dir_all(&dir)?;

    // V2a: if the version was published with a packed bundle, download and
    // unpack the whole folder. Legacy versions (packed_s3_key=None) take
    // the three-text-field write path below.
    let mode_label: &str;
    let written: Vec<PathBuf>;
    if version.packed_s3_key.is_some() {
        let dl = api_skills::bundle_download(&client, &skill.id, &version.version).await?;
        let resp = reqwest::Client::new().get(&dl.url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(CliError::Server {
                status: status.as_u16(),
                code: "BUNDLE_DOWNLOAD_FAILED".into(),
                message: format!("R2 GET returned {status}"),
            });
        }
        let bytes = resp.bytes().await?;
        let manifest = unpack_skill(&bytes, &dir)?;
        written = manifest
            .files
            .keys()
            .map(|arcname| dir.join(arcname.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .collect();
        mode_label = "bundle";
    } else {
        let mut acc = Vec::new();
        acc.extend(write_if_changed(&dir.join("SKILL.md"), &version.skill_md)?);
        acc.extend(write_if_changed(
            &dir.join("intuition.md"),
            &version.intuition_md,
        )?);
        acc.extend(write_if_changed(
            &dir.join("meta.knack.yaml"),
            &version.meta_yaml,
        )?);
        written = acc;
        mode_label = "legacy";
    }

    chatter(
        mode,
        format!(
            "pulled {}@{} ({}) → {}",
            skill.slug,
            version.version,
            mode_label,
            dir.display()
        ),
    );

    emit_ok(
        mode,
        json!({
            "skill_id": skill.id,
            "slug": skill.slug,
            "version": version.version,
            "path": dir,
            "files_written": written,
            "mode": mode_label,
        }),
        || {
            println!("✓ {}@{} → {}", skill.slug, version.version, dir.display());
        },
    );
    Ok(())
}

/// Splits a pull target into `(slug-or-handle, version)`.
///
/// Accepted shapes:
///
///   * `slug`                     → `(slug, None)`
///   * `slug@1.2.0`                → `(slug, Some("1.2.0"))`
///   * `@author/slug`             → `("@author/slug", None)`
///   * `@author/slug@1.2.0`        → `("@author/slug", Some("1.2.0"))`
///
/// The leading `@` of a handle is preserved so `find_by_slug` can route
/// to the marketplace resolver. Only an `@` that appears *after* a
/// leading-handle prefix counts as the version separator.
fn parse_slug_at_version(s: &str) -> (&str, Option<&str>) {
    let lookup_start = usize::from(s.starts_with('@'));
    if let Some(rel) = s[lookup_start..].find('@') {
        let abs = lookup_start + rel;
        return (&s[..abs], Some(&s[abs + 1..]));
    }
    (s, None)
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
    fn parse_handle_slug_no_version() {
        assert_eq!(
            parse_slug_at_version("@KnackOfficial/monthly-close"),
            ("@KnackOfficial/monthly-close", None)
        );
    }

    #[test]
    fn parse_handle_slug_with_version() {
        assert_eq!(
            parse_slug_at_version("@KnackOfficial/monthly-close@1.2.0"),
            ("@KnackOfficial/monthly-close", Some("1.2.0"))
        );
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
