//! `knack runs show <run-id>` — fetch one run as a single snapshot
//! with inputs, outputs, duration, and the note (if any).

use clap::Args;
use serde_json::json;

use crate::api::runs as api_runs;
use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The run-id printed by `knack run`.
    pub run_id: String,
}

pub async fn run(args: ShowArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_show(&args.run_id, local_path, mode);
    }
    cloud_show(args, client, mode).await
}

fn github_show(run_id: &str, local_path: &std::path::Path, mode: OutputMode) -> CliResult<()> {
    let snap = match knack_backend_github::find_run(local_path, run_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            let err = CliError::NotFound(format!(
                "run {} not found in last {} days",
                run_id,
                knack_backend_github::DEFAULT_LOOKBACK_DAYS
            ));
            emit_err(mode, &err);
            return Err(err);
        }
        Err(e) => {
            let err = CliError::Internal(format!("read run: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    emit_ok(
        mode,
        json!({
            "backend": "github",
            "run_id": snap.run_id,
            "skill": snap.skill,
            "version": snap.version,
            "agent": snap.agent,
            "status": snap.status,
            "note": snap.note,
            "inputs": snap.inputs,
            "outputs": snap.outputs,
            "started_at": snap.started_at,
            "completed_at": snap.completed_at,
            "duration_ms": snap.duration_ms,
        }),
        || {
            println!("run-id:      {}", snap.run_id);
            println!(
                "skill:       {}{}",
                snap.skill.as_deref().unwrap_or("-"),
                snap.version
                    .as_deref()
                    .map(|v| format!("@{v}"))
                    .unwrap_or_default()
            );
            println!("agent:       {}", snap.agent.as_deref().unwrap_or("-"));
            println!("status:      {}", snap.status);
            if let Some(ms) = snap.duration_ms {
                println!("duration:    {} ms", ms);
            }
            if let Some(s) = snap.started_at {
                println!("started:     {}", s.to_rfc3339());
            }
            if let Some(c) = snap.completed_at {
                println!("completed:   {}", c.to_rfc3339());
            }
            if !snap.inputs.is_empty() {
                println!("inputs:      {}", snap.inputs.join(", "));
            }
            if !snap.outputs.is_empty() {
                println!("outputs:     {}", snap.outputs.join(", "));
            }
            if let Some(n) = &snap.note {
                println!("note:        {n}");
            }
        },
    );
    Ok(())
}

async fn cloud_show(args: ShowArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let r = match api_runs::get(&client, &args.run_id).await {
        Ok(r) => r,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let duration_ms = r
        .finished_at
        .map(|f| (f - r.started_at).num_milliseconds().max(0));

    emit_ok(
        mode,
        json!({
            "backend": "cloud",
            "run_id": r.id,
            "skill_version_id": r.skill_version_id,
            "runtime": r.runtime,
            "agent_id": r.agent_id,
            "status": r.status,
            "started_at": r.started_at,
            "finished_at": r.finished_at,
            "duration_ms": duration_ms,
            "inputs_summary": r.inputs_summary,
            "outputs_summary": r.outputs_summary,
            "files_touched": r.files_touched,
            "marks": r.marks,
        }),
        || {
            println!("run-id:      {}", r.id);
            println!("status:      {}", r.status);
            println!(
                "runtime:     {}",
                r.runtime.as_deref().unwrap_or("-")
            );
            if let Some(ms) = duration_ms {
                println!("duration:    {} ms", ms);
            }
            println!("started:     {}", r.started_at.to_rfc3339());
            if let Some(f) = r.finished_at {
                println!("finished:    {}", f.to_rfc3339());
            }
            if !r.marks.is_empty() {
                println!("marks:");
                for m in &r.marks {
                    let status = m
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let note = m
                        .get("note")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("  · {status:10} {note}");
                }
            }
        },
    );
    Ok(())
}
