//! `knack runs trend <slug>` — time-bucketed stats. The axis is time;
//! the optional `--group-by` adds within-slice cross-tabs.
//!
//! Output shape (JSON):
//!
//!   {
//!     "interval": "day",
//!     "dimensions": ["version"],
//!     "window": {"since": "...", "until": "..."},
//!     "series": [
//!       {
//!         "bucket_start": "2026-05-27",
//!         "bucket_end":   "2026-05-27",
//!         "buckets": [
//!           {"key": {"version": "0.1.3"}, "total": 2, "succeeded": 1, ...}
//!         ]
//!       },
//!       ...
//!     ]
//!   }
//!
//! Every period in the window is emitted (zero-padding included) so
//! agents can render a gap-free sparkline without re-densifying.

use chrono::NaiveDate;
use clap::Args;
use serde_json::json;

use super::stats::parse_dimensions;
use super::{naive_to_utc, parse_window};
use crate::api::skills as api_skills;
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct TrendArgs {
    /// Skill slug (bare) or `@author/slug`.
    pub slug: String,

    /// Time bucket size. `day` aligns with JSONL day-files; `week`
    /// starts on Monday (ISO week, not US-Sunday).
    #[arg(long, default_value = "day")]
    pub interval: String,

    /// Comma-separated sub-grouping within each time slice. Empty
    /// means "one bucket per slice." Same dimension set as
    /// `runs stats --group-by`.
    #[arg(long)]
    pub group_by: Option<String>,

    /// Inclusive lower bound. `YYYY-MM-DD` or `<N>d`. Default: 30 days back.
    #[arg(long)]
    pub since: Option<String>,

    /// Inclusive upper bound. `YYYY-MM-DD` or `<N>d`. Default: today.
    #[arg(long)]
    pub until: Option<String>,
}

pub async fn run(args: TrendArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
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

    let interval = match knack_backend_github::TrendInterval::from_str(&args.interval) {
        Ok(i) => i,
        Err(e) => {
            let err = CliError::User {
                code: "BAD_INTERVAL".into(),
                message: e,
                hint: Some("supported: day, week".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let dims: Vec<String> = match args.group_by.as_deref() {
        Some(raw) => match parse_dimensions(raw) {
            Ok(d) => d,
            Err(e) => {
                let err = CliError::User {
                    code: "BAD_GROUP_BY".into(),
                    message: e,
                    hint: Some("dimensions: version, agent".into()),
                };
                emit_err(mode, &err);
                return Err(err);
            }
        },
        None => Vec::new(),
    };

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_trend(&args.slug, interval, &dims, local_path, since, until, mode);
    }
    cloud_trend(&args.slug, &args.interval, &dims, client, since, until, mode).await
}

fn github_trend(
    slug: &str,
    interval: knack_backend_github::TrendInterval,
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
    let series = knack_backend_github::group_time_buckets(&snapshots, interval, &dim_refs, since, until);

    let interval_str = match interval {
        knack_backend_github::TrendInterval::Day => "day",
        knack_backend_github::TrendInterval::Week => "week",
    };

    emit_ok(
        mode,
        json!({
            "backend": "github",
            "slug": slug,
            "interval": interval_str,
            "dimensions": dims,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "series": series,
        }),
        || render_series_human(slug, interval_str, dims, &series),
    );
    Ok(())
}

async fn cloud_trend(
    slug: &str,
    interval_raw: &str,
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

    let q = api_skills::TrendQuery {
        interval: interval_raw.to_string(),
        group_by: if dims.is_empty() {
            None
        } else {
            Some(dims.join(","))
        },
        include: vec!["p50".into(), "p95".into()],
        since: Some(naive_to_utc(since, false)),
        until: Some(naive_to_utc(until, true)),
    };
    let resp = match api_skills::get_trend(&client, &skill.id, &q).await {
        Ok(r) => r,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "slug": slug,
            "interval": interval_raw,
            "dimensions": dims,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "series": resp.series,
        }),
        || render_series_cloud(slug, interval_raw, dims, &resp.series),
    );
    Ok(())
}

fn key_label(dims: &[String], key: &std::collections::BTreeMap<String, Option<String>>) -> String {
    if dims.is_empty() {
        return "(all)".into();
    }
    dims.iter()
        .map(|d| key.get(d).and_then(|v| v.clone()).unwrap_or_else(|| "-".into()))
        .collect::<Vec<_>>()
        .join("/")
}

fn render_series_human(
    slug: &str,
    interval: &str,
    dims: &[String],
    series: &[knack_backend_github::TrendPoint],
) {
    println!("trend `{slug}`  interval={interval}");
    println!();
    println!(
        "{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
        "date", "cohort", "total", "ok", "fail", "rate"
    );
    for point in series {
        for b in &point.buckets {
            println!(
                "{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
                point.bucket_start,
                key_label(dims, &b.key),
                b.total,
                b.succeeded,
                b.failed,
                rate(b.success_rate),
            );
        }
        if point.buckets.is_empty() {
            println!("{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
                point.bucket_start, "(no runs)", 0, 0, 0, "-");
        }
    }
}

fn render_series_cloud(
    slug: &str,
    interval: &str,
    dims: &[String],
    series: &[api_skills::TrendPointDto],
) {
    println!("trend `{slug}`  interval={interval}");
    println!();
    println!(
        "{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
        "date", "cohort", "total", "ok", "fail", "rate"
    );
    for point in series {
        for b in &point.buckets {
            println!(
                "{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
                point.bucket_start,
                key_label(dims, &b.key),
                b.runs_total,
                b.runs_succeeded,
                b.runs_failed,
                rate(b.success_rate),
            );
        }
        if point.buckets.is_empty() {
            println!("{:<12} {:<24} {:>5} {:>5} {:>5} {:>8}",
                point.bucket_start, "(no runs)", 0, 0, 0, "-");
        }
    }
}

fn rate(r: Option<f64>) -> String {
    match r {
        Some(v) => format!("{:>6.1}%", v * 100.0),
        None => "-".into(),
    }
}
