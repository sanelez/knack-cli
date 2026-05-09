//! `knack edit <slug>` — open SKILL.md in $EDITOR; on save, push as new version.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use serde_json::json;

use crate::api::{ApiClient, skills as api_skills};
use crate::errors::{CliError, CliResult};
use crate::output::{OutputMode, chatter, emit_err, emit_ok};

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Skill slug to edit.
    pub slug: String,

    /// Editor command to invoke. Defaults to $EDITOR, then $VISUAL, then a
    /// reasonable platform default (`code -w` if available, else `vi` /
    /// `notepad`).
    #[arg(long)]
    pub editor: Option<String>,

    /// Write the new version with this semver. Default: bump patch.
    #[arg(long)]
    pub as_version: Option<String>,
}

pub async fn run(args: EditArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
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
        .ok_or_else(|| CliError::NotFound(format!("skill `{}` has no current version", args.slug)))?;
    let current = api_skills::get_version(&client, &skill.id, &current_semver).await?;

    let dir = tempfile::tempdir()?;
    let file = dir.path().join("SKILL.md");
    std::fs::write(&file, &current.skill_md)?;

    chatter(mode, format!("editing {} @ {}", args.slug, current.version));
    invoke_editor(&file, args.editor.as_deref())?;

    let edited = std::fs::read_to_string(&file)?;
    if edited == current.skill_md {
        emit_ok(
            mode,
            json!({
                "slug": args.slug,
                "changed": false,
                "version": current.version,
            }),
            || println!("(no changes)"),
        );
        return Ok(());
    }

    let next_semver = match args.as_version {
        Some(v) => v,
        None => bump_patch(&current.version)?,
    };

    let new_version = api_skills::create_version(
        &client,
        &skill.id,
        &api_skills::SkillVersionCreate {
            version: next_semver.clone(),
            skill_md: edited,
            intuition_md: current.intuition_md.clone(),
            meta_yaml: current.meta_yaml.clone(),
            parent_version_id: Some(current.id.clone()),
            artifact_ids: current.artifact_ids.clone(),
        },
    )
    .await?;

    emit_ok(
        mode,
        json!({
            "slug": args.slug,
            "skill_id": skill.id,
            "version": new_version.version,
            "parent_version_id": current.id,
            "changed": true,
        }),
        || println!("✓ {}@{} → {}", args.slug, current.version, new_version.version),
    );
    Ok(())
}

fn invoke_editor(path: &std::path::Path, override_cmd: Option<&str>) -> CliResult<()> {
    let cmd = override_cmd
        .map(str::to_string)
        .or_else(|| std::env::var("EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
        .unwrap_or_else(default_editor);

    // Split into argv. `EDITOR="code -w"` is a real-world case.
    let mut parts = cmd.split_whitespace();
    let bin = parts.next().ok_or_else(|| CliError::User {
        code: "EDIT_NO_EDITOR".into(),
        message: "no editor configured".into(),
        hint: Some("set $EDITOR or pass --editor".into()),
    })?;
    let mut command = Command::new(bin);
    for arg in parts {
        command.arg(arg);
    }
    command.arg(path);

    let status = command.status().map_err(|e| CliError::User {
        code: "EDIT_LAUNCH".into(),
        message: format!("couldn't launch editor `{cmd}`: {e}"),
        hint: Some("set $EDITOR to a working program".into()),
    })?;

    if !status.success() {
        return Err(CliError::User {
            code: "EDIT_NONZERO".into(),
            message: format!("editor exited with status {status}"),
            hint: None,
        });
    }
    Ok(())
}

fn default_editor() -> String {
    if cfg!(target_os = "windows") {
        "notepad".into()
    } else {
        "vi".into()
    }
}

#[allow(dead_code)]
fn _path_to_pathbuf(p: &std::path::Path) -> PathBuf {
    p.to_path_buf()
}

/// `1.0.0` → `1.0.1`. Mirrors the Python helper at
/// `apps/api/knack_api/services/skills.py:bump_patch`.
pub fn bump_patch(semver: &str) -> CliResult<String> {
    let s = semver.strip_prefix('v').unwrap_or(semver);
    let parts: Vec<&str> = s.split('.').collect();
    let (a, b, c) = match parts.as_slice() {
        [a, b] => (a, b, "0"),
        [a, b, c] => (a, b, *c),
        _ => {
            return Err(CliError::User {
                code: "EDIT_BAD_SEMVER".into(),
                message: format!("can't parse semver `{semver}`"),
                hint: None,
            });
        }
    };
    let parse = |x: &str| {
        x.parse::<u64>().map_err(|_| CliError::User {
            code: "EDIT_BAD_SEMVER".into(),
            message: format!("non-numeric component in `{semver}`"),
            hint: None,
        })
    };
    let (a, b, c) = (parse(a)?, parse(b)?, parse(c)?);
    Ok(format!("{}.{}.{}", a, b, c + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

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
