//! `knack publish <slug> [--major|--minor|--patch]` — push the local skill
//! folder as a new version.

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct PublishArgs {
    pub slug: String,

    /// Folder containing SKILL.md (default: `~/.knack/skills/<slug>/`).
    #[arg(long)]
    pub from: Option<PathBuf>,

    /// Override version. Otherwise inferred from --major/--minor/--patch.
    #[arg(long, conflicts_with_all = ["major", "minor", "patch"])]
    pub as_version: Option<String>,

    #[arg(long)]
    pub major: bool,
    #[arg(long)]
    pub minor: bool,
    #[arg(long)]
    pub patch: bool,
}

pub async fn run(args: PublishArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let dir = args
        .from
        .clone()
        .unwrap_or_else(|| client.config.skills_dir.join(&args.slug));

    let skill_md = read_required(&dir.join("SKILL.md"))?;
    let intuition_md = read_optional(&dir.join("intuition.md"));
    let meta_yaml = read_optional(&dir.join("meta.knack.yaml"));

    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let current_semver = skill
        .current_version_semver
        .clone()
        .unwrap_or_else(|| "0.0.0".into());

    // Default bump is patch (--patch flag is documented but not load-bearing —
    // its absence still picks the patch path).
    let next = match (args.as_version.clone(), args.major, args.minor) {
        (Some(v), _, _) => v,
        (_, true, _) => bump(&current_semver, BumpKind::Major)?,
        (_, _, true) => bump(&current_semver, BumpKind::Minor)?,
        _ => bump_patch(&current_semver)?,
    };

    let parent_id = skill.current_version_id.clone();
    let new_version = api_skills::create_version(
        &client,
        &skill.id,
        &api_skills::SkillVersionCreate {
            version: next.clone(),
            skill_md,
            intuition_md,
            meta_yaml,
            parent_version_id: parent_id.clone(),
            artifact_ids: vec![],
        },
    )
    .await?;

    emit_ok(
        mode,
        json!({
            "slug": args.slug,
            "skill_id": skill.id,
            "version": new_version.version,
            "parent_version_id": parent_id,
            "from": dir,
        }),
        || println!("✓ {}@{} published", args.slug, new_version.version),
    );
    Ok(())
}

#[derive(Copy, Clone)]
enum BumpKind {
    Major,
    Minor,
}

/// `1.0.0` → `1.0.1`. Mirrors the Python helper at
/// `apps/api/knack_api/services/skills.py:bump_patch`.
fn bump_patch(semver: &str) -> CliResult<String> {
    let s = semver.strip_prefix('v').unwrap_or(semver);
    let parts: Vec<&str> = s.split('.').collect();
    let (a, b, c) = match parts.as_slice() {
        [a, b] => (a, b, "0"),
        [a, b, c] => (a, b, *c),
        _ => {
            return Err(CliError::User {
                code: "PUBLISH_BAD_SEMVER".into(),
                message: format!("can't parse semver `{semver}`"),
                hint: None,
            });
        }
    };
    let parse = |x: &str| {
        x.parse::<u64>().map_err(|_| CliError::User {
            code: "PUBLISH_BAD_SEMVER".into(),
            message: format!("non-numeric component in `{semver}`"),
            hint: None,
        })
    };
    let (a, b, c) = (parse(a)?, parse(b)?, parse(c)?);
    Ok(format!("{}.{}.{}", a, b, c + 1))
}

fn bump(semver: &str, kind: BumpKind) -> CliResult<String> {
    let s = semver.strip_prefix('v').unwrap_or(semver);
    let parts: Vec<&str> = s.split('.').collect();
    let parse = |x: &str| {
        x.parse::<u64>().map_err(|_| CliError::User {
            code: "PUBLISH_BAD_SEMVER".into(),
            message: format!("non-numeric component in `{semver}`"),
            hint: None,
        })
    };
    let (a, b, _c) = match parts.as_slice() {
        [a, b] => (parse(a)?, parse(b)?, 0_u64),
        [a, b, c] => (parse(a)?, parse(b)?, parse(c)?),
        _ => {
            return Err(CliError::User {
                code: "PUBLISH_BAD_SEMVER".into(),
                message: format!("can't parse `{semver}`"),
                hint: None,
            });
        }
    };
    let bumped = match kind {
        BumpKind::Major => format!("{}.0.0", a + 1),
        BumpKind::Minor => format!("{}.{}.0", a, b + 1),
    };
    Ok(bumped)
}

fn read_required(path: &Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError::User {
        code: "PUBLISH_MISSING_FILE".into(),
        message: format!("can't read {}: {e}", path.display()),
        hint: Some("did you `knack pull` first, or write SKILL.md to the folder?".into()),
    })
}

fn read_optional(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_major() {
        assert_eq!(bump("1.2.3", BumpKind::Major).unwrap(), "2.0.0");
    }

    #[test]
    fn bump_minor() {
        assert_eq!(bump("1.2.3", BumpKind::Minor).unwrap(), "1.3.0");
        assert_eq!(bump("1.2", BumpKind::Minor).unwrap(), "1.3.0");
    }

    #[test]
    fn bump_rejects_garbage() {
        assert!(bump("nope", BumpKind::Major).is_err());
    }

    #[test]
    fn bump_patch_basic() {
        assert_eq!(bump_patch("1.0.0").unwrap(), "1.0.1");
        assert_eq!(bump_patch("0.1").unwrap(), "0.1.1");
        assert_eq!(bump_patch("v2.3.4").unwrap(), "2.3.5");
    }

    #[test]
    fn bump_patch_rejects_garbage() {
        assert!(bump_patch("not-a-version").is_err());
        assert!(bump_patch("1.x.0").is_err());
    }
}
