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

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::CliResult;
use crate::output::{display_path, emit_err, emit_ok, OutputMode};
use crate::skill_pack::unpack_skill;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Output directory. Created if missing. Defaults to
    /// `./knack-export-<YYYY-MM-DD>/` so multiple runs don't clobber.
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
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        PathBuf::from(format!("./knack-export-{today}"))
    });
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

    let http = reqwest::Client::new();
    for skill in &page.items {
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
        let resp = match http.get(&dl.url).send().await {
            Ok(r) => r,
            Err(e) => {
                skipped.push((skill.slug.clone(), format!("{e}")));
                continue;
            }
        };
        if !resp.status().is_success() {
            skipped.push((
                skill.slug.clone(),
                format!("R2 GET returned {}", resp.status()),
            ));
            continue;
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                skipped.push((skill.slug.clone(), format!("{e}")));
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
