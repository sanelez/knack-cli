//! `knack runs list <slug>` — page filtered runs newest-first.

use chrono::NaiveDate;
use clap::Args;
use serde_json::json;

use super::{naive_to_utc, parse_window};
use crate::api::runs as api_runs;
use crate::api::skills as api_skills;
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Skill slug (bare) or `@author/slug`. In self-host mode we resolve
    /// against the local clone; in cloud mode against the marketplace.
    pub slug: String,

    /// Filter to runs with this final status. Self-host accepts
    /// `started|succeeded|failed|aborted`; cloud accepts
    /// `running|succeeded|failed|unknown`.
    #[arg(long)]
    pub status: Option<String>,

    /// Filter to runs pinned to this skill version.
    #[arg(long)]
    pub version: Option<String>,

    /// Filter to runs tagged with this `--runtime` agent label.
    #[arg(long)]
    pub agent: Option<String>,

    /// Substring filter on the failure note (case-insensitive).
    #[arg(long)]
    pub note_contains: Option<String>,

    /// Inclusive lower bound. Accepts `YYYY-MM-DD` or `<N>d` (days ago).
    /// Defaults to 30 days back.
    #[arg(long)]
    pub since: Option<String>,

    /// Inclusive upper bound. Accepts `YYYY-MM-DD` or `<N>d` (days ago).
    /// Defaults to today.
    #[arg(long)]
    pub until: Option<String>,

    /// Cap the number of rows returned (default 50).
    #[arg(long, default_value_t = 50)]
    pub limit: u32,

    /// Cloud-only: page through results. Pass the previous response's
    /// `next_cursor` to fetch the next page. Self-host returns everything
    /// in the window in one shot.
    #[arg(long)]
    pub cursor: Option<String>,
}

pub async fn run(args: ListArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let (since, until) = match parse_window(args.since.as_deref(), args.until.as_deref()) {
        Ok(w) => w,
        Err(e) => {
            let err = CliError::User {
                code: "BAD_DATE".into(),
                message: e,
                hint: Some("use YYYY-MM-DD or `<N>d` (e.g. `7d`)".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_list(&args, local_path, since, until, mode);
    }
    cloud_list(args, client, since, until, mode).await
}

fn github_list(
    args: &ListArgs,
    local_path: &std::path::Path,
    since: NaiveDate,
    until: NaiveDate,
    mode: OutputMode,
) -> CliResult<()> {
    let (slug, _v) = crate::slug::parse_slug_at_version(&args.slug);
    let snapshots = match knack_backend_github::scan_snapshots(local_path, Some(slug), since, until)
    {
        Ok(s) => s,
        Err(e) => {
            let err = CliError::Internal(format!("scan runs: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let needle = args.note_contains.as_deref().map(|s| s.to_lowercase());

    let items: Vec<_> = snapshots
        .into_iter()
        .filter(|s| match &args.status {
            Some(want) => s.status.eq_ignore_ascii_case(want),
            None => true,
        })
        .filter(|s| match &args.version {
            Some(want) => s.version.as_deref() == Some(want.as_str()),
            None => true,
        })
        .filter(|s| match &args.agent {
            Some(want) => s.agent.as_deref() == Some(want.as_str()),
            None => true,
        })
        .filter(|s| match &needle {
            Some(n) => s
                .note
                .as_deref()
                .map(|note| note.to_lowercase().contains(n))
                .unwrap_or(false),
            None => true,
        })
        .take(args.limit as usize)
        .collect();

    let json_items: Vec<_> = items
        .iter()
        .map(|s| {
            json!({
                "run_id": s.run_id,
                "skill": s.skill,
                "version": s.version,
                "agent": s.agent,
                "status": s.status,
                "note": s.note,
                "inputs": s.inputs,
                "outputs": s.outputs,
                "started_at": s.started_at,
                "completed_at": s.completed_at,
                "duration_ms": s.duration_ms,
            })
        })
        .collect();

    let total = items.len();
    emit_ok(
        mode,
        json!({
            "backend": "github",
            "slug": slug,
            "items": json_items,
            "count": total,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "next_cursor": serde_json::Value::Null,
        }),
        || {
            if items.is_empty() {
                println!("no runs in window {} → {}", since, until);
                return;
            }
            println!(
                "{:<13} {:<8} {:<10} {:<10} dur(ms)  note",
                "run-id", "status", "version", "agent"
            );
            for s in &items {
                let id_short = s.run_id.get(..12).unwrap_or(&s.run_id);
                let dur = s
                    .duration_ms
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into());
                let note = s.note.as_deref().unwrap_or("");
                println!(
                    "{:<13} {:<8} {:<10} {:<10} {:<8} {}",
                    id_short,
                    truncate(&s.status, 8),
                    truncate(s.version.as_deref().unwrap_or("-"), 10),
                    truncate(s.agent.as_deref().unwrap_or("-"), 10),
                    dur,
                    truncate(note, 60),
                );
            }
        },
    );
    Ok(())
}

async fn cloud_list(
    args: ListArgs,
    client: ApiClient,
    since: NaiveDate,
    until: NaiveDate,
    mode: OutputMode,
) -> CliResult<()> {
    let (slug, _v) = crate::slug::parse_slug_at_version(&args.slug);
    let skill = match api_skills::find_by_slug(&client, slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{slug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    // Resolve --version to a skill_version_id once, up front. Run rows
    // carry skill_version_id but NOT the semver text, so without this
    // lookup the filter would have to choose between a no-op and N+1
    // requests. Single GET /skills/{id}/versions/{semver} is cheap.
    let version_filter_id: Option<String> = match args.version.as_deref() {
        Some(semver) => match api_skills::get_version(&client, &skill.id, semver).await {
            Ok(v) => Some(v.id),
            Err(CliError::NotFound(_)) => {
                let err = CliError::NotFound(format!(
                    "skill `{slug}` has no version `{semver}`"
                ));
                emit_err(mode, &err);
                return Err(err);
            }
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        },
        None => None,
    };

    let q = api_runs::RunsListQuery {
        status: args.status.clone(),
        marked_by: None,
        since: Some(naive_to_utc(since, false)),
        until: Some(naive_to_utc(until, true)),
        limit: Some(args.limit),
        cursor: args.cursor.clone(),
    };
    let page = match api_runs::list_for_skill(&client, &skill.id, &q).await {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let needle = args.note_contains.as_deref().map(|s| s.to_lowercase());

    let items: Vec<_> = page
        .items
        .into_iter()
        .filter(|r| match &args.agent {
            Some(a) => r.runtime.as_deref() == Some(a.as_str()),
            None => true,
        })
        .filter(|r| match &version_filter_id {
            Some(vid) => r.skill_version_id == *vid,
            None => true,
        })
        .filter(|r| match &needle {
            Some(n) => r
                .marks
                .iter()
                .filter_map(|m| m.get("note").and_then(|v| v.as_str()))
                .any(|note| note.to_lowercase().contains(n)),
            None => true,
        })
        .collect();

    let count = items.len();
    let next_cursor = page.next_cursor.clone();
    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "slug": slug,
            "items": items,
            "count": count,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "next_cursor": next_cursor,
        }),
        || {
            if items.is_empty() {
                println!("no runs in window {} → {}", since, until);
                return;
            }
            println!(
                "{:<13} {:<10} {:<12} dur(ms)  marks",
                "run-id", "status", "runtime"
            );
            for r in &items {
                let id_short = r.id.get(..12).unwrap_or(&r.id);
                let dur = match (r.started_at, r.finished_at) {
                    (s, Some(f)) => ((f - s).num_milliseconds()).to_string(),
                    _ => "-".into(),
                };
                println!(
                    "{:<13} {:<10} {:<12} {:<8} {}",
                    id_short,
                    truncate(&r.status, 10),
                    truncate(r.runtime.as_deref().unwrap_or("-"), 12),
                    dur,
                    r.marks.len(),
                );
            }
            if let Some(ref cursor) = next_cursor {
                println!();
                println!("more results: --cursor {}", cursor);
            }
        },
    );
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}
