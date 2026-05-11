//! `knack create <slug> --name "..."` — bootstrap a new skill shell.
//!
//! Hits `POST /skills` to register a new slug + name + scope. Returns the
//! generated skill id. After this, the caller writes SKILL.md / intuition.md
//! to disk and runs `knack publish <slug>` to push the first immutable
//! version. Separating the two steps keeps each command single-purpose and
//! makes the agent-driven flow easy to script.

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Slug for the new skill. Lowercase, hyphens, no leading hyphen
    /// (matches `^[a-z0-9][a-z0-9-]*$`).
    pub slug: String,

    /// Display name (1-200 chars).
    #[arg(long)]
    pub name: String,

    /// Visibility scope. Defaults to `personal`. `team` requires --team-id.
    #[arg(long, default_value = "personal")]
    pub scope: String,

    /// Team UUID (required when --scope team, forbidden otherwise).
    #[arg(long)]
    pub team_id: Option<String>,
}

pub async fn run(args: CreateArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    validate_slug(&args.slug)?;
    validate_scope(&args.scope, args.team_id.as_deref())?;

    let body = api_skills::SkillCreate {
        slug: args.slug.clone(),
        name: args.name.clone(),
        scope: Some(args.scope.clone()),
        owner_team_id: args.team_id.clone(),
    };

    let skill = match api_skills::create(&client, &body).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    emit_ok(
        mode,
        json!({
            "slug": skill.slug,
            "skill_id": skill.id,
            "scope": skill.scope,
            "name": skill.name,
        }),
        || {
            println!("✓ created {} (id: {})", skill.slug, skill.id);
            println!(
                "next: write SKILL.md (rules go in its ## Intuition section), \
                 then run `knack publish {} --from <dir>`",
                skill.slug,
            );
        },
    );
    Ok(())
}

fn validate_slug(slug: &str) -> CliResult<()> {
    if slug.is_empty() || slug.len() > 100 {
        return Err(CliError::User {
            code: "CREATE_BAD_SLUG".into(),
            message: format!("slug `{slug}` must be 1-100 chars"),
            hint: None,
        });
    }
    let mut chars = slug.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(CliError::User {
            code: "CREATE_BAD_SLUG".into(),
            message: format!("slug must start with [a-z0-9], got `{slug}`"),
            hint: Some("use lowercase letters, numbers, and hyphens".into()),
        });
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(CliError::User {
                code: "CREATE_BAD_SLUG".into(),
                message: format!("slug contains invalid character `{c}` in `{slug}`"),
                hint: Some("only [a-z0-9-] allowed after the first character".into()),
            });
        }
    }
    Ok(())
}

fn validate_scope(scope: &str, team_id: Option<&str>) -> CliResult<()> {
    match scope {
        "personal" | "public" => {
            if team_id.is_some() {
                return Err(CliError::User {
                    code: "CREATE_BAD_SCOPE".into(),
                    message: format!("--team-id is forbidden when scope=`{scope}`"),
                    hint: None,
                });
            }
            Ok(())
        }
        "team" => {
            if team_id.is_none() {
                return Err(CliError::User {
                    code: "CREATE_BAD_SCOPE".into(),
                    message: "--team-id is required when scope=`team`".into(),
                    hint: Some("pass --team-id <uuid>".into()),
                });
            }
            Ok(())
        }
        other => Err(CliError::User {
            code: "CREATE_BAD_SCOPE".into(),
            message: format!("scope must be one of personal | team | public, got `{other}`"),
            hint: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_accepts_canonical() {
        validate_slug("intake-cleanup").unwrap();
        validate_slug("a").unwrap();
        validate_slug("9-lives").unwrap();
        validate_slug("month-end-close-2026").unwrap();
    }

    #[test]
    fn slug_rejects_uppercase() {
        assert!(validate_slug("Intake-Cleanup").is_err());
    }

    #[test]
    fn slug_rejects_leading_hyphen() {
        assert!(validate_slug("-foo").is_err());
    }

    #[test]
    fn slug_rejects_underscore() {
        assert!(validate_slug("intake_cleanup").is_err());
    }

    #[test]
    fn slug_rejects_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn scope_personal_no_team() {
        validate_scope("personal", None).unwrap();
        assert!(validate_scope("personal", Some("abc")).is_err());
    }

    #[test]
    fn scope_team_requires_id() {
        assert!(validate_scope("team", None).is_err());
        validate_scope("team", Some("uuid")).unwrap();
    }

    #[test]
    fn scope_public_no_team() {
        validate_scope("public", None).unwrap();
        assert!(validate_scope("public", Some("abc")).is_err());
    }

    #[test]
    fn scope_rejects_unknown() {
        assert!(validate_scope("private", None).is_err());
    }
}
