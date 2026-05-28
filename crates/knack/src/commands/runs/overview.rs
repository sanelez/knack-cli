//! `knack runs overview` — portfolio dashboard.
//!
//! Self-host walks `<clone>/skills/<slug>/` directories, builds a
//! per-skill rollup, flags regressions (current version vs prior) and
//! staleness (zero runs in window). Cloud calls the new
//! `GET /me/runs/overview` endpoint.
//!
//! Output shape (JSON):
//!
//!   {
//!     "window": { "since": "...", "until": "..." },
//!     "skills": [
//!       {
//!         "slug": "email-triage",
//!         "current_version": "0.1.4",
//!         "runs_total": 22,
//!         "succeeded": 20, "failed": 2,
//!         "success_rate": 0.909,
//!         "p50_ms": 165, "p95_ms": 340,
//!         "last_run_at": "...",
//!         "regression": {
//!           "current_version": "0.1.4",
//!           "prior_version": "0.1.3",
//!           "delta_success_rate": -0.083,
//!           "current_success_rate": 0.909,
//!           "prior_success_rate": 0.992
//!         },
//!         "stale": false
//!       }
//!     ],
//!     "summary": {
//!       "skills_total": 7,
//!       "skills_stale": 2,
//!       "regressions": ["email-triage", "summarize"]
//!     }
//!   }

use chrono::NaiveDate;
use clap::Args;
use serde_json::json;

use super::{naive_to_utc, parse_window};
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct OverviewArgs {
    /// Inclusive lower bound. `YYYY-MM-DD` or `<N>d`. Default: 30 days back.
    #[arg(long)]
    pub since: Option<String>,

    /// Inclusive upper bound. `YYYY-MM-DD` or `<N>d`. Default: today.
    #[arg(long)]
    pub until: Option<String>,

    /// Drop skills with fewer than N runs in window. Default 0
    /// (include everything; `stale` field on each row tells the agent
    /// whether to act on it).
    #[arg(long, default_value_t = 0)]
    pub min_runs: u64,
}

pub async fn run(args: OverviewArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
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
        return github_overview(local_path, since, until, args.min_runs, mode);
    }
    cloud_overview(client, since, until, args.min_runs, mode).await
}

fn list_local_skills(clone_root: &std::path::Path) -> Vec<String> {
    let skills_dir = clone_root.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        // Filter out hidden / metadata directories so the overview
        // doesn't surface dotfile cruft as "skills."
        .filter(|name| !name.starts_with('.'))
        .collect();
    out.sort();
    out
}

fn github_overview(
    local_path: &std::path::Path,
    since: NaiveDate,
    until: NaiveDate,
    min_runs: u64,
    mode: OutputMode,
) -> CliResult<()> {
    let slugs = list_local_skills(local_path);
    let mut skills: Vec<knack_backend_github::SkillOverview> = Vec::new();

    for slug in &slugs {
        let snaps = match knack_backend_github::scan_snapshots(local_path, Some(slug), since, until)
        {
            Ok(s) => s,
            Err(e) => {
                let err = CliError::Internal(format!("scan runs for `{slug}`: {e}"));
                emit_err(mode, &err);
                return Err(err);
            }
        };
        let overview = knack_backend_github::build_overview(slug, &snaps);
        if overview.runs_total >= min_runs {
            skills.push(overview);
        }
    }

    let summary = build_summary(&skills);

    emit_ok(
        mode,
        json!({
            "backend": "github",
            "window": { "since": since.to_string(), "until": until.to_string() },
            "skills": skills,
            "summary": summary,
        }),
        || render_overview_gh(&skills, &summary, since, until),
    );
    Ok(())
}

async fn cloud_overview(
    client: ApiClient,
    since: NaiveDate,
    until: NaiveDate,
    min_runs: u64,
    mode: OutputMode,
) -> CliResult<()> {
    let q = crate::api::overview::OverviewQuery {
        since: Some(naive_to_utc(since, false)),
        until: Some(naive_to_utc(until, true)),
        min_runs,
    };
    let resp = match crate::api::overview::get_overview(&client, &q).await {
        Ok(r) => r,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let summary = json!({
        "skills_total": resp.skills.len(),
        "skills_stale": resp.skills.iter().filter(|s| s.stale).count(),
        "regressions": resp.skills.iter()
            .filter(|s| s.regression.is_some())
            .map(|s| s.slug.clone())
            .collect::<Vec<_>>(),
    });

    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "window": { "since": since.to_string(), "until": until.to_string() },
            "skills": resp.skills,
            "summary": summary,
        }),
        || render_overview_cloud(&resp.skills, &summary, since, until),
    );
    Ok(())
}

fn build_summary(skills: &[knack_backend_github::SkillOverview]) -> serde_json::Value {
    json!({
        "skills_total": skills.len(),
        "skills_stale": skills.iter().filter(|s| s.stale).count(),
        "regressions": skills.iter()
            .filter(|s| s.regression.is_some())
            .map(|s| s.slug.clone())
            .collect::<Vec<_>>(),
    })
}

fn render_overview_gh(
    skills: &[knack_backend_github::SkillOverview],
    summary: &serde_json::Value,
    since: NaiveDate,
    until: NaiveDate,
) {
    if skills.is_empty() {
        println!("no skills under the local clone");
        return;
    }
    println!("overview · {} → {}", since, until);
    println!();
    println!(
        "{:<24} {:>10} {:>7} {:>9} {:>8} flags",
        "skill", "version", "runs", "rate", "p50(ms)"
    );
    for s in skills {
        let flag = if s.regression.is_some() {
            " REGRESSION"
        } else if s.stale {
            " stale"
        } else {
            ""
        };
        println!(
            "{:<24} {:>10} {:>7} {:>9} {:>8}{}",
            truncate(&s.slug, 24),
            s.current_version.as_deref().unwrap_or("-"),
            s.runs_total,
            s.success_rate
                .map(|r| format!("{:>6.1}%", r * 100.0))
                .unwrap_or_else(|| "-".into()),
            s.p50_ms
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".into()),
            flag,
        );
    }
    println!();
    if let Some(regs) = summary.get("regressions").and_then(|v| v.as_array()) {
        if !regs.is_empty() {
            println!("regressions: {}", regs.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "));
        }
    }
}

fn render_overview_cloud(
    skills: &[crate::api::overview::SkillOverviewDto],
    summary: &serde_json::Value,
    since: NaiveDate,
    until: NaiveDate,
) {
    if skills.is_empty() {
        println!("no skills");
        return;
    }
    println!("overview · {} → {}", since, until);
    println!();
    println!(
        "{:<24} {:>10} {:>7} {:>9} {:>8} flags",
        "skill", "version", "runs", "rate", "p50(ms)"
    );
    for s in skills {
        let flag = if s.regression.is_some() {
            " REGRESSION"
        } else if s.stale {
            " stale"
        } else {
            ""
        };
        println!(
            "{:<24} {:>10} {:>7} {:>9} {:>8}{}",
            truncate(&s.slug, 24),
            s.current_version.as_deref().unwrap_or("-"),
            s.runs_total,
            s.success_rate
                .map(|r| format!("{:>6.1}%", r * 100.0))
                .unwrap_or_else(|| "-".into()),
            s.p50_ms
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".into()),
            flag,
        );
    }
    println!();
    if let Some(regs) = summary.get("regressions").and_then(|v| v.as_array()) {
        if !regs.is_empty() {
            println!("regressions: {}", regs.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "));
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}
