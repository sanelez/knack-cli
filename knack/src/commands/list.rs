//! `knack list [--scope=public]` — list skills.

use clap::Args;
use console::style;
use serde_json::json;

use crate::api::{ApiClient, skills as api_skills};
use crate::errors::{CliError, CliResult};
use crate::output::{OutputMode, emit_err, emit_ok};

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by scope: personal, team, or public.
    #[arg(long, value_parser = ["personal", "team", "public"])]
    pub scope: Option<String>,

    /// Page size cap. Defaults to 50, max 200.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

pub async fn run(args: ListArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let page = match api_skills::list(&client, args.scope.as_deref(), None, args.limit).await {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return match e {
                CliError::AuthFailed(_) => Err(CliError::AuthRequired),
                other => Err(other),
            };
        }
    };

    emit_ok(
        mode,
        json!({
            "items": page.items,
            "next_cursor": page.next_cursor,
        }),
        || {
            if page.items.is_empty() {
                println!("(no skills yet — `knack interview --local` to make one)");
                return;
            }
            for s in &page.items {
                let semver = s.current_version_semver.as_deref().unwrap_or("—");
                println!(
                    "{:<28} {:<8} {}",
                    s.slug,
                    style(semver).cyan(),
                    style(&s.scope).dim()
                );
            }
        },
    );
    Ok(())
}
