//! `knack export [--to <dir>]` — bulk-pull every skill in the caller's
//! library to a local directory.
//!
//! The lock-in tell that this fix removes: the only way to leave Knack
//! Cloud was per-skill `knack pull`. With 100 skills that's 100 round
//! trips and no manifest of what was exported. Export resolves that:
//! one call enumerates everything the caller can read, then the client
//! fans out via the existing bundle endpoint.
//!
//! Self-host has no equivalent surface because the user already has the
//! files. We just print where they live.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{display_path, emit_err, emit_ok, OutputMode};
use crate::skill_pack::unpack_skill;

/// Per-skill R2 GET timeout. The presigned URL is one-shot and
/// transient; if the connection hasn't started returning bytes within
/// 60s something is wrong (hotel wifi, R2 issue, MITM). Time out
/// instead of stranding the user with a silent hang.
const BUNDLE_GET_TIMEOUT_S: u64 = 60;

/// Per-skill retry attempts on transient errors. With backoff `1s, 2s,
/// 4s` total tail = 7s before we give up on one skill. Re-raises 4xx
/// without retry — those are auth/access problems where retry won't
/// help.
const BUNDLE_GET_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Output directory. Created if missing. Defaults to
    /// `./knack-export-<YYYY-MM-DDTHH-MM-SS>/` — the seconds precision
    /// means double-runs in one day don't collide regardless of
    /// `--force`.
    #[arg(long)]
    pub to: Option<PathBuf>,

    /// Scope filter. Without it, exports everything the caller can read
    /// (personal + team + own public). Useful when migrating just one
    /// scope off cloud.
    #[arg(long, value_parser = ["personal", "team", "public"])]
    pub scope: Option<String>,

    /// Pagination cap when listing. Default 200; bump for large libraries.
    #[arg(long, default_value_t = 200)]
    pub limit: u32,

    /// Re-export into a target directory that already exists and isn't
    /// empty. Without this flag the CLI refuses, so a typo in `--to`
    /// can't silently clobber a previous export.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(args: ExportArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_export_pointer(local_path, mode);
    }
    cloud_export(args, client, mode).await
}

fn github_export_pointer(local_path: &std::path::Path, mode: OutputMode) -> CliResult<()> {
    let skills_dir = local_path.join("skills");
    emit_ok(
        mode,
        json!({
            "backend": "github",
            "skills_dir": skills_dir.display().to_string(),
            "message": "self-host already keeps every skill file in the local clone — copy the skills/ subtree",
        }),
        || {
            println!("self-host: your skills live at {}", display_path(&skills_dir));
            println!("  to export, copy that directory anywhere you want.");
        },
    );
    Ok(())
}

async fn cloud_export(
    args: ExportArgs,
    client: ApiClient,
    mode: OutputMode,
) -> CliResult<()> {
    let target_root = args.to.clone().unwrap_or_else(|| {
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        PathBuf::from(format!("./knack-export-{ts}"))
    });

    // Refuse to write into an existing non-empty directory unless --force.
    // The seconds-precision default target makes accidental collisions
    // basically impossible, but the user can `--to ./foo` themselves into
    // one, and a typo into a real project dir would be catastrophic.
    if !args.force {
        if let Some(reason) = nonempty_dir_reason(&target_root) {
            let err = CliError::User {
                code: "EXPORT_TARGET_EXISTS".into(),
                message: format!(
                    "refusing to export into `{}`: {reason}",
                    target_root.display()
                ),
                hint: Some("pass `--force` to overwrite, or pick a different `--to <dir>`".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    }

    std::fs::create_dir_all(&target_root)?;

    let page = match api_skills::list_with_folder(
        &client,
        args.scope.as_deref(),
        None,
        false,
        args.limit,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let mut exported: Vec<String> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(BUNDLE_GET_TIMEOUT_S))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // Progress bar — but only render in human mode. `--json` agents
    // would choke on the ANSI escapes / re-draws and they're streaming
    // the data envelope anyway.
    let progress = if mode.json {
        None
    } else {
        let bar = ProgressBar::new(page.items.len() as u64);
        bar.set_style(
            ProgressStyle::with_template("  {spinner} [{pos}/{len}] {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        Some(bar)
    };

    for skill in &page.items {
        if let Some(ref bar) = progress {
            bar.inc(1);
            bar.set_message(format!("{}/{}", skill.scope, skill.slug));
        }
        let semver = match skill.current_version_semver.clone() {
            Some(v) => v,
            None => {
                skipped.push((skill.slug.clone(), "no published version".into()));
                continue;
            }
        };
        let version = match api_skills::get_version(&client, &skill.id, &semver).await {
            Ok(v) => v,
            Err(e) => {
                skipped.push((skill.slug.clone(), format!("{e}")));
                continue;
            }
        };
        if version.packed_s3_key.is_none() {
            skipped.push((skill.slug.clone(), "legacy version (no bundle)".into()));
            continue;
        }
        let dl = match api_skills::bundle_download(&client, &skill.id, &version.version).await {
            Ok(d) => d,
            Err(e) => {
                skipped.push((skill.slug.clone(), format!("{e}")));
                continue;
            }
        };
        let bytes = match fetch_with_retry(&http, &dl.url).await {
            Ok(b) => b,
            Err(e) => {
                skipped.push((skill.slug.clone(), e));
                continue;
            }
        };
        // Scope subdir keeps personal/team/public from colliding when the
        // same slug exists across two scopes (legal — slug uniqueness is
        // per-owner).
        let scope_dir = target_root.join(&skill.scope);
        let dir = scope_dir.join(&skill.slug);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            skipped.push((skill.slug.clone(), format!("mkdir: {e}")));
            continue;
        }
        if let Err(e) = unpack_skill(&bytes, &dir) {
            skipped.push((skill.slug.clone(), format!("unpack: {e}")));
            continue;
        }
        exported.push(format!(
            "{}/{} (v{})",
            skill.scope, skill.slug, version.version
        ));
    }
    if let Some(bar) = progress {
        bar.finish_and_clear();
    }

    let total = page.items.len();
    let succeeded = exported.len();
    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "target": target_root.display().to_string(),
            "exported": exported,
            "skipped": skipped.iter().map(|(slug, why)| json!({"slug": slug, "reason": why})).collect::<Vec<_>>(),
            "total_seen": total,
            "scope_filter": args.scope,
            "more_available": page.next_cursor.is_some(),
        }),
        || {
            println!("exported {}/{} skill(s) to {}", succeeded, total, display_path(&target_root));
            for line in &exported {
                println!("  ✓ {}", line);
            }
            for (slug, why) in &skipped {
                println!("  - {}: {}", slug, why);
            }
            if page.next_cursor.is_some() {
                println!();
                println!("note: library exceeded --limit ({}). Re-run with a higher --limit to capture the rest.", args.limit);
            }
        },
    );
    Ok(())
}

/// Return `Some(reason)` if `target` exists and is non-empty, or
/// `None` if the path is safe to write into. Treats a missing path as
/// safe (we'll `create_dir_all` it). Existing file at the path (not a
/// directory) is also considered unsafe — we'd error later on
/// `create_dir_all` anyway, but surfacing the reason here is friendlier.
fn nonempty_dir_reason(target: &Path) -> Option<String> {
    let meta = match std::fs::metadata(target) {
        Ok(m) => m,
        Err(_) => return None, // doesn't exist → safe
    };
    if !meta.is_dir() {
        return Some("path exists but is not a directory".into());
    }
    let mut entries = match std::fs::read_dir(target) {
        Ok(e) => e,
        Err(e) => return Some(format!("read_dir failed: {e}")),
    };
    if entries.next().is_some() {
        Some("directory exists and is not empty".into())
    } else {
        None
    }
}

/// Fetch `url` (a presigned R2 GET) with retry on transient errors.
///
/// Retries on network errors and 5xx responses with exponential backoff
/// (1s → 2s → 4s, capped at `BUNDLE_GET_MAX_ATTEMPTS`). 4xx responses
/// are auth/access problems — retry won't help, so we surface them
/// immediately. The reqwest client itself already enforces a per-request
/// timeout via the `.timeout()` builder, so a hung connection won't
/// block forever.
async fn fetch_with_retry(http: &reqwest::Client, url: &str) -> Result<bytes::Bytes, String> {
    let mut last_err = String::from("unknown failure");
    for attempt in 1..=BUNDLE_GET_MAX_ATTEMPTS {
        match http.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp.bytes().await.map_err(|e| format!("read body: {e}"));
                }
                if status.is_client_error() {
                    // 4xx — retry won't help. Bail with the status.
                    return Err(format!("R2 GET returned {status} (no retry on 4xx)"));
                }
                last_err = format!("R2 GET returned {status} (attempt {attempt})");
            }
            Err(e) => {
                last_err = format!("network error: {e} (attempt {attempt})");
            }
        }
        if attempt < BUNDLE_GET_MAX_ATTEMPTS {
            // Backoff: 1s, 2s, 4s (the last sleep is unreachable since
            // we break before it; kept for symmetry if MAX_ATTEMPTS bumps).
            let secs = 1u64 << (attempt - 1);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    }
    Err(last_err)
}
