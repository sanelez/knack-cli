//! `knack edit <slug> [--name X] [--description Y] [--scope personal|public]`
//!
//! Wraps PATCH /skills/{id}. Slug is immutable server-side by design — keep
//! it stable so share links and agent context files never break.

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Slug of the skill to edit (your own personal or team skills only).
    pub slug: String,

    /// New display name (1-200 chars).
    #[arg(long)]
    pub name: Option<String>,

    /// New one-line description (0-280 chars).
    #[arg(long)]
    pub description: Option<String>,

    /// New visibility scope. `public` requires a claimed username and counts
    /// against the `max_public_skills` plan quota.
    #[arg(long, value_parser = ["personal", "public"])]
    pub scope: Option<String>,
}

pub async fn run(args: EditArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if args.name.is_none() && args.description.is_none() && args.scope.is_none() {
        let err = CliError::User {
            code: "EDIT_NO_CHANGES".into(),
            message: "no fields supplied. pass at least one of --name / --description / --scope".into(),
            hint: Some("slug is immutable; if you want a different slug, create a new skill".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let body = api_skills::SkillUpdate {
        name: args.name.clone(),
        description: args.description.clone(),
        scope: args.scope.clone(),
    };

    let updated = match api_skills::update(&client, &skill.id, &body).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    emit_ok(
        mode,
        json!({
            "slug": updated.slug,
            "skill_id": updated.id,
            "name": updated.name,
            "description": updated.description,
            "scope": updated.scope,
        }),
        || {
            let mut changes: Vec<String> = Vec::new();
            if args.name.is_some() {
                changes.push(format!("name → {:?}", updated.name));
            }
            if args.description.is_some() {
                changes.push(format!("description → {:?}", updated.description));
            }
            if args.scope.is_some() {
                changes.push(format!("scope → {}", updated.scope));
            }
            println!("✓ {} updated: {}", updated.slug, changes.join(", "));
        },
    );
    Ok(())
}
