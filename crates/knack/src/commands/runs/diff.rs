//! `knack runs diff <slug> <ver-a> <ver-b>` — side-by-side cohort
//! comparison: total / success rate / p50 / p95 / top failure notes.
//!
//! Computed client-side from the same per-version buckets that
//! `runs stats` returns, so the contract stays minimal and the two
//! commands can't disagree.

use chrono::NaiveDate;
use clap::Args;
use serde_json::json;

use super::{naive_to_utc, parse_window};
use crate::api::skills as api_skills;
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Skill slug (bare) or `@author/slug`.
    pub slug: String,

    /// Baseline version (left column).
    pub version_a: String,

    /// Comparison version (right column).
    pub version_b: String,

    /// Inclusive lower bound. `YYYY-MM-DD` or `<N>d`. Default: 30 days back.
    #[arg(long)]
    pub since: Option<String>,

    /// Inclusive upper bound. `YYYY-MM-DD` or `<N>d`. Default: today.
    #[arg(long)]
    pub until: Option<String>,
}

#[derive(Default, Clone)]
struct Bucket {
    version: String,
    /// `false` when the cohort had zero rows in the window — used to
    /// suppress misleading "0% → 90%, +90 pp" deltas in the human
    /// renderer and to surface a `present: false` field in `--json`.
    present: bool,
    total: u64,
    succeeded: u64,
    failed: u64,
    success_rate: Option<f64>,
    p50_ms: Option<u64>,
    p95_ms: Option<u64>,
    top_notes: Vec<(String, u64)>,
}

pub async fn run(args: DiffArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
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

    let (a, b) = if let BackendMode::Github { local_path, .. } = &client.config.backend {
        match github_buckets(&args, local_path, since, until) {
            Ok(pair) => pair,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    } else {
        match cloud_buckets(&args, &client, since, until).await {
            Ok(pair) => pair,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    };

    // Only emit numeric deltas when both cohorts have at least one row.
    // Otherwise the math (e.g. `0 → 0.9 = +0.9`) implies a regression
    // that isn't real — the "before" side simply never ran.
    let delta = if a.present && b.present {
        json!({
            "success_rate": delta_opt(a.success_rate, b.success_rate),
            "p50_ms": delta_opt(a.p50_ms.map(|x| x as f64), b.p50_ms.map(|x| x as f64)),
            "p95_ms": delta_opt(a.p95_ms.map(|x| x as f64), b.p95_ms.map(|x| x as f64)),
            "runs_total": b.total as i64 - a.total as i64,
        })
    } else {
        json!(null)
    };

    emit_ok(
        mode,
        json!({
            "slug": args.slug,
            "window": { "since": since.to_string(), "until": until.to_string() },
            "a": bucket_json(&a),
            "b": bucket_json(&b),
            "delta": delta,
        }),
        || render_human(&args.slug, &a, &b, since, until),
    );
    Ok(())
}

fn github_buckets(
    args: &DiffArgs,
    local_path: &std::path::Path,
    since: NaiveDate,
    until: NaiveDate,
) -> Result<(Bucket, Bucket), CliError> {
    let (slug, _v) = crate::slug::parse_slug_at_version(&args.slug);
    let snapshots = knack_backend_github::scan_snapshots(local_path, Some(slug), since, until)
        .map_err(|e| CliError::Internal(format!("scan runs: {e}")))?;
    let buckets = knack_backend_github::group_buckets(&snapshots, &["version"]);

    Ok((
        bucket_from_gh(&args.version_a, &buckets),
        bucket_from_gh(&args.version_b, &buckets),
    ))
}

fn bucket_from_gh(version: &str, buckets: &[knack_backend_github::StatsBucket]) -> Bucket {
    let Some(b) = buckets
        .iter()
        .find(|b| b.key.get("version").and_then(|v| v.as_deref()) == Some(version))
    else {
        return Bucket {
            version: version.to_string(),
            present: false,
            ..Default::default()
        };
    };
    Bucket {
        version: version.to_string(),
        present: b.total > 0,
        total: b.total,
        succeeded: b.succeeded,
        failed: b.failed,
        success_rate: b.success_rate,
        p50_ms: b.p50_ms,
        p95_ms: b.p95_ms,
        top_notes: b.top_notes.iter().map(|n| (n.note.clone(), n.count)).collect(),
    }
}

async fn cloud_buckets(
    args: &DiffArgs,
    client: &ApiClient,
    since: NaiveDate,
    until: NaiveDate,
) -> Result<(Bucket, Bucket), CliError> {
    let (slug, _v) = crate::slug::parse_slug_at_version(&args.slug);
    let skill = api_skills::find_by_slug(client, slug)
        .await?
        .ok_or_else(|| CliError::NotFound(format!("skill `{slug}` not found")))?;
    let q = api_skills::StatsQuery {
        group_by: Some("version".into()),
        include: vec!["p50".into(), "p95".into()],
        since: Some(naive_to_utc(since, false)),
        until: Some(naive_to_utc(until, true)),
    };
    let resp = api_skills::get_stats(client, &skill.id, &q).await?;
    let buckets = match resp {
        api_skills::SkillStatsResponse::ByVersion(b) => b.buckets,
        api_skills::SkillStatsResponse::Flat(_) => {
            return Err(CliError::User {
                code: "STATS_UNSUPPORTED".into(),
                message: "server does not yet support per-version stats".into(),
                hint: Some(
                    "upgrade the API to a build that accepts `?group_by=version`, or use \
                     self-host mode where the CLI does the rollup locally"
                        .into(),
                ),
            })
        }
    };

    Ok((
        bucket_from_cloud(&args.version_a, &buckets),
        bucket_from_cloud(&args.version_b, &buckets),
    ))
}

fn bucket_from_cloud(version: &str, buckets: &[api_skills::StatsBucketDto]) -> Bucket {
    let Some(b) = buckets
        .iter()
        .find(|b| b.key.get("version").and_then(|v| v.as_deref()) == Some(version))
    else {
        return Bucket {
            version: version.to_string(),
            present: false,
            ..Default::default()
        };
    };
    Bucket {
        version: version.to_string(),
        present: b.runs_total > 0,
        total: b.runs_total,
        succeeded: b.runs_succeeded,
        failed: b.runs_failed,
        success_rate: b.success_rate,
        p50_ms: b.p50_ms,
        p95_ms: b.p95_ms,
        top_notes: b
            .top_notes
            .iter()
            .map(|n| (n.note.clone(), n.count))
            .collect(),
    }
}

fn render_human(slug: &str, a: &Bucket, b: &Bucket, since: NaiveDate, until: NaiveDate) {
    println!(
        "diff `{slug}`  {} → {}  ({since} → {until})",
        a.version, b.version
    );
    println!();
    if !a.present {
        println!("note: no runs for `{}` in this window", a.version);
    }
    if !b.present {
        println!("note: no runs for `{}` in this window", b.version);
    }
    if !a.present || !b.present {
        // Suppress the full row table — one side has no data, so deltas
        // would be misleading. The JSON envelope still carries the raw
        // shape (with `present: false`) for callers that want it.
        return;
    }

    println!(
        "{:<14} {:>12} {:>12} {:>12}",
        "metric", a.version, b.version, "Δ"
    );
    println!("{}", "-".repeat(54));
    print_row("total", a.total as f64, b.total as f64, FmtKind::Int);
    print_row(
        "succeeded",
        a.succeeded as f64,
        b.succeeded as f64,
        FmtKind::Int,
    );
    print_row("failed", a.failed as f64, b.failed as f64, FmtKind::Int);
    print_row_opt(
        "success rate",
        a.success_rate,
        b.success_rate,
        FmtKind::Pct,
    );
    print_row_opt(
        "p50 (ms)",
        a.p50_ms.map(|x| x as f64),
        b.p50_ms.map(|x| x as f64),
        FmtKind::Int,
    );
    print_row_opt(
        "p95 (ms)",
        a.p95_ms.map(|x| x as f64),
        b.p95_ms.map(|x| x as f64),
        FmtKind::Int,
    );

    if !a.top_notes.is_empty() {
        println!();
        println!("top failure notes · {}", a.version);
        for (note, count) in &a.top_notes {
            println!("  {:>3}× {}", count, note);
        }
    }
    if !b.top_notes.is_empty() {
        println!();
        println!("top failure notes · {}", b.version);
        for (note, count) in &b.top_notes {
            println!("  {:>3}× {}", count, note);
        }
    }
}

#[derive(Copy, Clone)]
enum FmtKind {
    Int,
    Pct,
}

fn print_row(label: &str, a: f64, b: f64, kind: FmtKind) {
    let delta = b - a;
    println!(
        "{:<14} {:>12} {:>12} {:>12}",
        label,
        fmt(a, kind),
        fmt(b, kind),
        fmt_signed(delta, kind),
    );
}

fn print_row_opt(label: &str, a: Option<f64>, b: Option<f64>, kind: FmtKind) {
    let delta = match (a, b) {
        (Some(x), Some(y)) => Some(y - x),
        _ => None,
    };
    println!(
        "{:<14} {:>12} {:>12} {:>12}",
        label,
        fmt_opt(a, kind),
        fmt_opt(b, kind),
        delta.map(|d| fmt_signed(d, kind)).unwrap_or("-".into()),
    );
}

fn fmt(v: f64, kind: FmtKind) -> String {
    match kind {
        FmtKind::Int => format!("{:.0}", v),
        FmtKind::Pct => format!("{:.1}%", v * 100.0),
    }
}

fn fmt_opt(v: Option<f64>, kind: FmtKind) -> String {
    v.map(|x| fmt(x, kind)).unwrap_or("-".into())
}

fn fmt_signed(v: f64, kind: FmtKind) -> String {
    let sign = if v > 0.0 { "+" } else { "" };
    match kind {
        FmtKind::Int => format!("{sign}{:.0}", v),
        FmtKind::Pct => format!("{sign}{:.1} pp", v * 100.0),
    }
}

fn delta_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(y - x),
        _ => None,
    }
}

fn bucket_json(b: &Bucket) -> serde_json::Value {
    json!({
        "version": b.version,
        "present": b.present,
        "runs_total": b.total,
        "runs_succeeded": b.succeeded,
        "runs_failed": b.failed,
        "success_rate": b.success_rate,
        "p50_ms": b.p50_ms,
        "p95_ms": b.p95_ms,
        "top_notes": b.top_notes.iter().map(|(n, c)| json!({"note": n, "count": c})).collect::<Vec<_>>(),
    })
}
