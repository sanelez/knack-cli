//! `knack runs stats <slug>` — group runs by one or more dimensions
//! and report totals, success rate, p50/p95 duration, last-run-at, and
//! the top failure notes per cohort.
//!
//! `--group-by` accepts a comma-separated list. Today's dimensions:
//! `version`, `agent`. The cross-tab grouping (`--group-by
//! version,agent`) is what answers "did the patch help every agent,
//! or only claude-code?"

use chrono::NaiveDate;
use clap::Args;
use serde_json::json;

use super::{naive_to_utc, parse_window};
use crate::api::skills as api_skills;
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

/// Dimensions the aggregator and the server route both understand.
const KNOWN_DIMS: &[&str] = &["version", "agent"];

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Skill slug (bare) or `@author/slug`.
    pub slug: String,

    /// Comma-separated cohort key. Today: `version`, `agent`, or
    /// `version,agent` for cross-tab. Defaults to `version`.
    #[arg(long, default_value = "version")]
    pub group_by: String,

    /// Inclusive lower bound. `YYYY-MM-DD` or `<N>d`. Default: 30 days back.
    #[arg(long)]
    pub since: Option<String>,

    /// Inclusive upper bound. `YYYY-MM-DD` or `<N>d`. Default: today.
    #[arg(long)]
    pub until: Option<String>,
}

pub async fn run(args: StatsArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
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

    let dims: Vec<String> = match parse_dimensions(&args.group_by) {
        Ok(d) => d,
        Err(e) => {
            let err = CliError::User {
                code: "BAD_GROUP_BY".into(),
                message: e,
                hint: Some(format!("dimensions: {}", KNOWN_DIMS.join(", "))),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_stats(&args.slug, &dims, local_path, since, until, mode);
    }
    cloud_stats(&args.slug, &dims, client, since, until, mode).await
}

/// Validate `--group-by`. Returns owned strings so the caller doesn't
/// need to keep `args.group_by` alive.
pub(crate) fn parse_dimensions(raw: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !KNOWN_DIMS.contains(&trimmed) {
            return Err(format!(
                "unknown dimension `{trimmed}` (try: {})",
                KNOWN_DIMS.join(", ")
            ));
        }
        if out.iter().any(|d: &String| d == trimmed) {
            // Idempotent dedup; better than erroring on `version,version`.
            continue;
        }
        out.push(trimmed.to_string());
    }
    if out.is_empty() {
        return Err("at least one dimension required".into());
    }
    Ok(out)
}

fn github_stats(
    slug: &str,
    dims: &[String],
    local_path: &std::path::Path,
    since: NaiveDate,
    until: NaiveDate,
    mode: OutputMode,
) -> CliResult<()> {
    let (slug, _v) = crate::slug::parse_slug_at_version(slug);
    let snapshots = match knack_backend_github::scan_snapshots(local_path, Some(slug), since, until)
    {
        Ok(s) => s,
        Err(e) => {
            let err = CliError::Internal(format!("scan runs: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };
    let dim_refs: Vec<&str> = dims.iter().map(|s| s.as_str()).collect();
    let buckets = knack_backend_github::group_buckets(&snapshots, &dim_refs);

    emit_ok(
        mode,
        json!({
            "backend": "github",
            "slug": slug,
            "dimensions": dims,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "buckets": buckets,
        }),
        || render_buckets_gh(slug, dims, &buckets, since, until),
    );
    Ok(())
}

async fn cloud_stats(
    slug: &str,
    dims: &[String],
    client: ApiClient,
    since: NaiveDate,
    until: NaiveDate,
    mode: OutputMode,
) -> CliResult<()> {
    let (slug, _v) = crate::slug::parse_slug_at_version(slug);
    let skill = match api_skills::find_by_slug(&client, slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{slug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let q = api_skills::StatsQuery {
        group_by: Some(dims.join(",")),
        include: vec!["p50".into(), "p95".into()],
        since: Some(naive_to_utc(since, false)),
        until: Some(naive_to_utc(until, true)),
    };
    let resp = match api_skills::get_stats(&client, &skill.id, &q).await {
        Ok(r) => r,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let buckets = match resp {
        api_skills::SkillStatsResponse::ByVersion(b) => b.buckets,
        api_skills::SkillStatsResponse::Flat(flat) => {
            // Server is on an older build that doesn't yet support
            // group_by. Surface the flat numbers as a single bucket so
            // the contract stays consistent and `--json` is still useful.
            let mut key = std::collections::BTreeMap::new();
            for d in dims {
                key.insert(d.clone(), None);
            }
            vec![api_skills::StatsBucketDto {
                key,
                runs_total: flat.runs_total,
                runs_succeeded: flat.runs_succeeded,
                runs_failed: flat.runs_failed,
                runs_unmarked: flat.runs_unmarked,
                success_rate: flat.success_rate,
                p50_ms: None,
                p95_ms: None,
                last_run_at: flat.last_run_at,
                top_notes: vec![],
            }]
        }
    };

    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "slug": slug,
            "dimensions": dims,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "buckets": buckets,
        }),
        || render_buckets_cloud(slug, dims, &buckets, since, until),
    );
    Ok(())
}

fn key_label(dims: &[String], key: &std::collections::BTreeMap<String, Option<String>>) -> String {
    dims.iter()
        .map(|d| key.get(d).and_then(|v| v.clone()).unwrap_or_else(|| "-".into()))
        .collect::<Vec<_>>()
        .join("/")
}

fn render_buckets_gh(
    slug: &str,
    dims: &[String],
    buckets: &[knack_backend_github::StatsBucket],
    since: NaiveDate,
    until: NaiveDate,
) {
    if buckets.is_empty() {
        println!("no runs for `{slug}` in window {since} → {until}");
        return;
    }
    println!(
        "stats for `{slug}`  by {}  ({since} → {until})",
        dims.join("/")
    );
    println!();
    println!(
        "{:<24} {:>7} {:>7} {:>7} {:>9} {:>9} {:>9}",
        dims.join("/"),
        "total",
        "ok",
        "fail",
        "rate",
        "p50(ms)",
        "p95(ms)"
    );
    for b in buckets {
        println!(
            "{:<24} {:>7} {:>7} {:>7} {:>9} {:>9} {:>9}",
            key_label(dims, &b.key),
            b.total,
            b.succeeded,
            b.failed,
            rate(b.success_rate),
            opt_u64(b.p50_ms),
            opt_u64(b.p95_ms),
        );
    }
    for b in buckets {
        if b.top_notes.is_empty() {
            continue;
        }
        println!();
        println!("top failure notes · {}", key_label(dims, &b.key));
        for n in &b.top_notes {
            println!("  {:>3}× {}", n.count, n.note);
        }
    }
}

fn render_buckets_cloud(
    slug: &str,
    dims: &[String],
    buckets: &[api_skills::StatsBucketDto],
    since: NaiveDate,
    until: NaiveDate,
) {
    if buckets.is_empty() {
        println!("no runs for `{slug}` in window {since} → {until}");
        return;
    }
    println!(
        "stats for `{slug}`  by {}  ({since} → {until})",
        dims.join("/")
    );
    println!();
    println!(
        "{:<24} {:>7} {:>7} {:>7} {:>9} {:>9} {:>9}",
        dims.join("/"),
        "total",
        "ok",
        "fail",
        "rate",
        "p50(ms)",
        "p95(ms)"
    );
    for b in buckets {
        println!(
            "{:<24} {:>7} {:>7} {:>7} {:>9} {:>9} {:>9}",
            key_label(dims, &b.key),
            b.runs_total,
            b.runs_succeeded,
            b.runs_failed,
            rate(b.success_rate),
            opt_u64(b.p50_ms),
            opt_u64(b.p95_ms),
        );
    }
    for b in buckets {
        if b.top_notes.is_empty() {
            continue;
        }
        println!();
        println!("top failure notes · {}", key_label(dims, &b.key));
        for n in &b.top_notes {
            println!("  {:>3}× {}", n.count, n.note);
        }
    }
}

fn rate(r: Option<f64>) -> String {
    match r {
        Some(v) => format!("{:>6.1}%", v * 100.0),
        None => "-".into(),
    }
}

fn opt_u64(v: Option<u64>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "-".into(),
    }
}
