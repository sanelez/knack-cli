//! `knack mark <run_id> <succeeded|failed> [--note=…]` — close the agent loop.
//!
//! `status` is positional now (matches the form agent.txt teaches). The
//! legacy `--status=` flag is still accepted as a deprecated synonym so
//! anyone scripting the old form keeps working.

use clap::Args;
use serde_json::json;

use crate::api::{runs as api_runs, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Run id (UUID) — `knack run` prints it on every invocation. Pass a
    /// comma-separated list (`a,b,c`) to mark several runs in one call;
    /// the same `--note` / `--output` applies to every id.
    pub run_id: String,

    /// Outcome. Positional — preferred form: `knack mark <run-id> succeeded`.
    /// Optional here because `--status=` is also accepted for backward compat;
    /// at runtime we require exactly one of the two forms.
    #[arg(value_parser = ["succeeded", "failed"])]
    pub outcome: Option<String>,

    /// Legacy: `--status=succeeded|failed`. Prefer the positional form.
    /// Hidden from --help so new users see the positional form first.
    #[arg(long = "status", value_parser = ["succeeded", "failed"], hide = true, conflicts_with = "outcome")]
    pub status_flag: Option<String>,

    /// Free-form note. For `failed`, the skill author gets notified with this
    /// text — be specific.
    #[arg(long)]
    pub note: Option<String>,

    /// Alias for `--note`. Spec uses `--reason` for failures.
    #[arg(long, conflicts_with = "note")]
    pub reason: Option<String>,

    /// Output file path. Repeatable: pass `--output` once per file the run
    /// produced. Captured into the run's `outputs` array in the telemetry
    /// log so a future audit knows what artifacts came out.
    #[arg(long)]
    pub output: Vec<std::path::PathBuf>,

    /// Self-host only: skip the auto `git push origin main` that follows
    /// the telemetry commit. The local commit still lands so the next
    /// pushed event catches up. `KNACK_AUTO_PUSH=0` is the equivalent
    /// env-level kill switch; either disables the network hop.
    #[arg(long)]
    pub no_push: bool,
}

pub async fn run(args: MarkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let status = match args.outcome.clone().or(args.status_flag.clone()) {
        Some(s) => s,
        None => {
            let err = CliError::User {
                code: "MARK_MISSING_STATUS".into(),
                message: "missing status".into(),
                hint: Some("knack mark <run-id> succeeded   (or `failed`)".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let note = args.note.clone().or(args.reason.clone());
    let outputs: Vec<String> = args
        .output
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let run_ids: Vec<String> = args
        .run_id
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if run_ids.is_empty() {
        let err = CliError::User {
            code: "MARK_MISSING_RUN_ID".into(),
            message: "no run ids supplied".into(),
            hint: Some("knack mark <run-id>[,<run-id>...] <status>".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        let push = super::run::resolve_push_flag(local_path, args.no_push);
        if run_ids.len() == 1 {
            return github_mark(
                &run_ids[0],
                &status,
                note.as_deref(),
                &outputs,
                local_path,
                push,
                mode,
            );
        }
        return github_mark_bulk(&run_ids, &status, note.as_deref(), &outputs, local_path, push, mode);
    }

    if run_ids.len() == 1 {
        return cloud_mark_single(&run_ids[0], &status, note.as_deref(), &client, mode).await;
    }
    cloud_mark_bulk(&run_ids, &status, note.as_deref(), &client, mode).await
}

async fn cloud_mark_single(
    run_id: &str,
    status: &str,
    note: Option<&str>,
    client: &ApiClient,
    mode: OutputMode,
) -> CliResult<()> {
    match api_runs::mark(
        client,
        run_id,
        &api_runs::RunMarkBody {
            status: status.to_string(),
            note: note.map(str::to_string),
        },
    )
    .await
    {
        Ok(run) => {
            emit_ok(
                mode,
                json!({
                    "run_id": run.id,
                    "status": status,
                    "note": note,
                    "marks_count": run.marks.len(),
                }),
                || println!("✓ marked {} {}", run.id, status),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn cloud_mark_bulk(
    run_ids: &[String],
    status: &str,
    note: Option<&str>,
    client: &ApiClient,
    mode: OutputMode,
) -> CliResult<()> {
    let mut succeeded: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for id in run_ids {
        let body = api_runs::RunMarkBody {
            status: status.to_string(),
            note: note.map(str::to_string),
        };
        match api_runs::mark(client, id, &body).await {
            Ok(_) => succeeded.push(id.clone()),
            Err(e) => failed.push((id.clone(), format!("{e}"))),
        }
    }
    let all_ok = failed.is_empty();
    emit_ok(
        mode,
        json!({
            "status": status,
            "note": note,
            "marked": succeeded,
            "failed": failed.iter().map(|(id, why)| json!({"run_id": id, "error": why})).collect::<Vec<_>>(),
            "total": run_ids.len(),
            "ok": all_ok,
        }),
        || {
            println!("marked {} run(s) {}", succeeded.len(), status);
            for (id, why) in &failed {
                println!("  ! {}: {}", id, why);
            }
        },
    );
    if all_ok {
        Ok(())
    } else {
        Err(CliError::Internal(format!(
            "{}/{} marks failed",
            failed.len(),
            run_ids.len()
        )))
    }
}

fn github_mark_bulk(
    run_ids: &[String],
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    local_path: &std::path::Path,
    push: bool,
    mode: OutputMode,
) -> CliResult<()> {
    let mut marked: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for id in run_ids {
        match knack_backend_github::mark_run(local_path, id, status, note, outputs, push) {
            Ok(_) => marked.push(id.clone()),
            Err(e) => failed.push((id.clone(), format!("{e}"))),
        }
    }
    let all_ok = failed.is_empty();
    emit_ok(
        mode,
        json!({
            "status": status,
            "note": note,
            "marked": marked,
            "failed": failed.iter().map(|(id, why)| json!({"run_id": id, "error": why})).collect::<Vec<_>>(),
            "total": run_ids.len(),
            "ok": all_ok,
            "backend": "github",
        }),
        || {
            println!("marked {} run(s) {}", marked.len(), status);
            for (id, why) in &failed {
                println!("  ! {}: {}", id, why);
            }
        },
    );
    if all_ok {
        Ok(())
    } else {
        Err(CliError::Internal(format!(
            "{}/{} marks failed",
            failed.len(),
            run_ids.len()
        )))
    }
}

fn github_mark(
    run_id: &str,
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    local_path: &std::path::Path,
    push: bool,
    mode: OutputMode,
) -> CliResult<()> {
    match knack_backend_github::mark_run(local_path, run_id, status, note, outputs, push) {
        Ok(snapshot) => {
            emit_ok(
                mode,
                json!({
                    "run_id": snapshot.run_id,
                    "status": snapshot.status,
                    "note": snapshot.note,
                    "skill": snapshot.skill,
                    "version": snapshot.version,
                    "agent": snapshot.agent,
                    "inputs": snapshot.inputs,
                    "outputs": snapshot.outputs,
                    "started_at": snapshot.started_at,
                    "completed_at": snapshot.completed_at,
                    "duration_ms": snapshot.duration_ms,
                    "backend": "github",
                }),
                || {
                    println!("✓ marked {} {}", snapshot.run_id, snapshot.status);
                    if let Some(ms) = snapshot.duration_ms {
                        println!("  duration: {} ms", ms);
                    }
                    if !snapshot.outputs.is_empty() {
                        println!("  outputs: {}", snapshot.outputs.join(", "));
                    }
                    if let Some(n) = &snapshot.note {
                        println!("  note: {n}");
                    }
                },
            );
            Ok(())
        }
        Err(e) => {
            let err = CliError::NotFound(format!("mark run: {e}"));
            emit_err(mode, &err);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    // The mark command's logic is mostly the API call; tests are covered by
    // CliError exit-code mapping in errors.rs and the integration tests.
    use crate::errors::CliError;
    use crate::output::err_envelope;

    #[test]
    fn mark_failed_envelope_includes_hint_when_present() {
        let err = CliError::PlanLimit {
            message: "too many marks".into(),
            hint: Some("upgrade".into()),
        };
        let env = err_envelope(&err);
        assert_eq!(env["error"]["code"], "PLAN_LIMIT_EXCEEDED");
        assert_eq!(env["error"]["hint"], "upgrade");
    }
}
