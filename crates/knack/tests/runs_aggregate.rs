//! Integration tests for the self-host `knack runs` aggregator paths.
//!
//! Drives `knack_backend_github` directly against a tempdir of
//! hand-crafted JSONL day-files. Covers scan, group_buckets (single +
//! cross-tab), group_time_buckets (daily/weekly), and the per-skill
//! overview with regression detection.

use chrono::{NaiveDate, TimeZone, Utc};
use knack_backend_github::{
    build_overview, detect_regression, group_buckets, group_time_buckets, scan_snapshots,
    TrendInterval,
};
use std::fs;
use std::io::Write;
use tempfile::tempdir;

fn write_day(repo: &std::path::Path, ym: &str, ymd: &str, lines: &[&str]) {
    let dir = repo.join("runs").join(ym);
    fs::create_dir_all(&dir).unwrap();
    let mut f = fs::File::create(dir.join(format!("{ymd}.jsonl"))).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn started(skill: &str, run: &str, version: &str, agent: &str, at: &str) -> String {
    format!(
        "{{\"event\":\"started\",\"run_id\":\"{run}\",\"skill\":\"{skill}\",\
         \"version\":\"{version}\",\"agent\":\"{agent}\",\"inputs\":[],\"outputs\":[],\
         \"status\":\"started\",\"at\":\"{at}\"}}"
    )
}

fn marked(
    skill: &str,
    run: &str,
    version: &str,
    agent: &str,
    status: &str,
    note: Option<&str>,
    at: &str,
) -> String {
    let note_str = note
        .map(|n| format!(",\"note\":\"{n}\""))
        .unwrap_or_default();
    format!(
        "{{\"event\":\"marked\",\"run_id\":\"{run}\",\"skill\":\"{skill}\",\
         \"version\":\"{version}\",\"agent\":\"{agent}\",\"inputs\":[],\"outputs\":[],\
         \"status\":\"{status}\"{note_str},\"at\":\"{at}\"}}"
    )
}

#[test]
fn scan_then_group_by_version_matches_smoke_test_expectations() {
    let dir = tempdir().unwrap();
    let repo = dir.path();

    write_day(
        repo,
        "2026-05",
        "2026-05-27",
        &[
            &started("triage", "r1", "0.1.3", "claude-code", "2026-05-27T10:00:00Z"),
            &marked(
                "triage", "r1", "0.1.3", "claude-code", "succeeded", None,
                "2026-05-27T10:00:00.250Z",
            ),
            &started("triage", "r2", "0.1.3", "claude-code", "2026-05-27T11:00:00Z"),
            &marked(
                "triage", "r2", "0.1.3", "claude-code", "failed",
                Some("timeout"), "2026-05-27T11:00:01Z",
            ),
        ],
    );
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "r3", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked(
                "triage", "r3", "0.1.4", "claude-code", "succeeded", None,
                "2026-05-28T10:00:00.180Z",
            ),
            &started("triage", "r4", "0.1.4", "cursor", "2026-05-28T11:00:00Z"),
            &marked(
                "triage", "r4", "0.1.4", "cursor", "succeeded", None,
                "2026-05-28T11:00:00.120Z",
            ),
        ],
    );

    let snapshots = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 27).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    assert_eq!(snapshots.len(), 4);

    let by_ver = group_buckets(&snapshots, &["version"]);
    let v013 = by_ver
        .iter()
        .find(|b| b.key.get("version").unwrap().as_deref() == Some("0.1.3"))
        .unwrap();
    let v014 = by_ver
        .iter()
        .find(|b| b.key.get("version").unwrap().as_deref() == Some("0.1.4"))
        .unwrap();
    assert_eq!(v013.total, 2);
    assert_eq!(v013.succeeded, 1);
    assert_eq!(v013.failed, 1);
    assert_eq!(v014.total, 2);
    assert_eq!(v014.succeeded, 2);
    assert_eq!(v013.top_notes[0].note, "timeout");
    assert_eq!(
        v013.last_run_at.unwrap().date_naive(),
        NaiveDate::from_ymd_opt(2026, 5, 27).unwrap()
    );
    assert!(v014.last_run_at.unwrap() > Utc.with_ymd_and_hms(2026, 5, 28, 11, 0, 0).unwrap());
}

#[test]
fn cross_tab_version_and_agent_isolates_per_agent_regression() {
    // 0.1.3: 2 claude-code succeeded, 0 cursor.
    // 0.1.4: 1 claude-code succeeded, 1 cursor failed (regression on cursor only).
    let dir = tempdir().unwrap();
    let repo = dir.path();
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "a", "0.1.3", "claude-code", "2026-05-28T08:00:00Z"),
            &marked(
                "triage", "a", "0.1.3", "claude-code", "succeeded", None,
                "2026-05-28T08:00:00.100Z",
            ),
            &started("triage", "b", "0.1.3", "claude-code", "2026-05-28T09:00:00Z"),
            &marked(
                "triage", "b", "0.1.3", "claude-code", "succeeded", None,
                "2026-05-28T09:00:00.100Z",
            ),
            &started("triage", "c", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked(
                "triage", "c", "0.1.4", "claude-code", "succeeded", None,
                "2026-05-28T10:00:00.100Z",
            ),
            &started("triage", "d", "0.1.4", "cursor", "2026-05-28T11:00:00Z"),
            &marked(
                "triage", "d", "0.1.4", "cursor", "failed",
                Some("schema mismatch"), "2026-05-28T11:00:00.500Z",
            ),
        ],
    );

    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let buckets = group_buckets(&snaps, &["version", "agent"]);

    // The cursor/0.1.4 bucket should be the only one with a failure.
    let cursor_014 = buckets
        .iter()
        .find(|b| {
            b.key.get("version").unwrap().as_deref() == Some("0.1.4")
                && b.key.get("agent").unwrap().as_deref() == Some("cursor")
        })
        .unwrap();
    assert_eq!(cursor_014.failed, 1);
    assert_eq!(cursor_014.success_rate, Some(0.0));
    assert_eq!(cursor_014.top_notes[0].note, "schema mismatch");
    // claude-code 0.1.4 should be 100% success.
    let claude_014 = buckets
        .iter()
        .find(|b| {
            b.key.get("version").unwrap().as_deref() == Some("0.1.4")
                && b.key.get("agent").unwrap().as_deref() == Some("claude-code")
        })
        .unwrap();
    assert_eq!(claude_014.success_rate, Some(1.0));
}

#[test]
fn trend_daily_emits_gap_free_series() {
    let dir = tempdir().unwrap();
    let repo = dir.path();
    // Three days: 5/26 empty, 5/27 has one run, 5/28 has one run.
    write_day(
        repo,
        "2026-05",
        "2026-05-27",
        &[
            &started("triage", "r1", "0.1.3", "claude-code", "2026-05-27T10:00:00Z"),
            &marked(
                "triage", "r1", "0.1.3", "claude-code", "succeeded", None,
                "2026-05-27T10:00:00.100Z",
            ),
        ],
    );
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "r2", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked(
                "triage", "r2", "0.1.4", "claude-code", "failed",
                Some("timeout"), "2026-05-28T10:00:00.500Z",
            ),
        ],
    );

    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 26).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let series = group_time_buckets(
        &snaps,
        TrendInterval::Day,
        &[],
        NaiveDate::from_ymd_opt(2026, 5, 26).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    );
    assert_eq!(series.len(), 3);
    // 5/26: empty buckets (no runs)
    assert_eq!(series[0].bucket_start, NaiveDate::from_ymd_opt(2026, 5, 26).unwrap());
    assert!(series[0].buckets.is_empty() || series[0].buckets[0].total == 0);
    // 5/27: one bucket with one succeeded run
    assert_eq!(series[1].bucket_start, NaiveDate::from_ymd_opt(2026, 5, 27).unwrap());
    assert_eq!(series[1].buckets[0].succeeded, 1);
    // 5/28: one bucket with one failed run
    assert_eq!(series[2].bucket_start, NaiveDate::from_ymd_opt(2026, 5, 28).unwrap());
    assert_eq!(series[2].buckets[0].failed, 1);
    assert_eq!(series[2].buckets[0].top_notes[0].note, "timeout");
}

#[test]
fn trend_weekly_buckets_align_to_monday() {
    let dir = tempdir().unwrap();
    let repo = dir.path();
    // 2026-05-25 is a Monday; 2026-05-28 is the Thursday of that week.
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "r1", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked(
                "triage", "r1", "0.1.4", "claude-code", "succeeded", None,
                "2026-05-28T10:00:00.100Z",
            ),
        ],
    );
    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 31).unwrap(),
    )
    .unwrap();
    let series = group_time_buckets(
        &snaps,
        TrendInterval::Week,
        &[],
        NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 31).unwrap(),
    );
    assert_eq!(series.len(), 1);
    assert_eq!(
        series[0].bucket_start,
        NaiveDate::from_ymd_opt(2026, 5, 25).unwrap()
    );
    assert_eq!(
        series[0].bucket_end,
        NaiveDate::from_ymd_opt(2026, 5, 31).unwrap()
    );
    assert_eq!(series[0].buckets[0].succeeded, 1);
}

#[test]
fn overview_detects_regression() {
    // 0.1.3 had 2 succeeded (100%); 0.1.4 has 1 succeeded + 1 failed (50%) — regression.
    let dir = tempdir().unwrap();
    let repo = dir.path();
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "a", "0.1.3", "claude-code", "2026-05-28T08:00:00Z"),
            &marked("triage", "a", "0.1.3", "claude-code", "succeeded", None, "2026-05-28T08:00:00.100Z"),
            &started("triage", "b", "0.1.3", "claude-code", "2026-05-28T09:00:00Z"),
            &marked("triage", "b", "0.1.3", "claude-code", "succeeded", None, "2026-05-28T09:00:00.100Z"),
            &started("triage", "c", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked("triage", "c", "0.1.4", "claude-code", "succeeded", None, "2026-05-28T10:00:00.100Z"),
            &started("triage", "d", "0.1.4", "claude-code", "2026-05-28T11:00:00Z"),
            &marked("triage", "d", "0.1.4", "claude-code", "failed", Some("regressed"), "2026-05-28T11:00:00.500Z"),
        ],
    );
    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let ov = build_overview("triage", &snaps);
    assert_eq!(ov.runs_total, 4);
    assert_eq!(ov.current_version.as_deref(), Some("0.1.4"));
    let reg = ov.regression.expect("expected regression flag");
    assert_eq!(reg.current_version, "0.1.4");
    assert_eq!(reg.prior_version, "0.1.3");
    assert!(reg.delta_success_rate < 0.0);
    assert_eq!(reg.current_success_rate, Some(0.5));
    assert_eq!(reg.prior_success_rate, Some(1.0));
    assert!(!ov.stale);
}

#[test]
fn overview_no_regression_when_only_one_version() {
    let dir = tempdir().unwrap();
    let repo = dir.path();
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "a", "0.1.0", "claude-code", "2026-05-28T08:00:00Z"),
            &marked("triage", "a", "0.1.0", "claude-code", "succeeded", None, "2026-05-28T08:00:00.100Z"),
        ],
    );
    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let ov = build_overview("triage", &snaps);
    assert!(ov.regression.is_none());
}

#[test]
fn overview_stale_when_no_runs() {
    let dir = tempdir().unwrap();
    let snaps = scan_snapshots(
        dir.path(),
        Some("never-run"),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let ov = build_overview("never-run", &snaps);
    assert!(ov.stale);
    assert_eq!(ov.runs_total, 0);
    assert!(ov.regression.is_none());
}

#[test]
fn detect_regression_returns_none_when_current_better() {
    // Build two buckets where the LATER version has a HIGHER success rate.
    let dir = tempdir().unwrap();
    let repo = dir.path();
    write_day(
        repo,
        "2026-05",
        "2026-05-28",
        &[
            &started("triage", "a", "0.1.3", "claude-code", "2026-05-28T08:00:00Z"),
            &marked("triage", "a", "0.1.3", "claude-code", "failed", Some("x"), "2026-05-28T08:00:00.100Z"),
            &started("triage", "b", "0.1.3", "claude-code", "2026-05-28T09:00:00Z"),
            &marked("triage", "b", "0.1.3", "claude-code", "succeeded", None, "2026-05-28T09:00:00.100Z"),
            &started("triage", "c", "0.1.4", "claude-code", "2026-05-28T10:00:00Z"),
            &marked("triage", "c", "0.1.4", "claude-code", "succeeded", None, "2026-05-28T10:00:00.100Z"),
        ],
    );
    let snaps = scan_snapshots(
        repo,
        Some("triage"),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
    )
    .unwrap();
    let buckets = group_buckets(&snaps, &["version"]);
    assert!(detect_regression(&buckets).is_none());
}
