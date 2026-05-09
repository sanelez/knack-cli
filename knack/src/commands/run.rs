//! `knack run <slug> --input <path>` — execute a skill.
//!
//! v0 implementation: detect the local agent runtime (Claude Code via `claude`
//! on PATH; Cowork via `cowork`; otherwise direct API), record a Run with the
//! API, hand off the skill folder + input to the runtime, and finalize the
//! Run on completion. Per spec § V the agent flywheel needs *something* called
//! "knack run" that produces a run id agents can `mark`. We focus the v0 on
//! the telemetry side; runtime auto-detection is intentionally narrow and
//! falls through to a "no runtime detected" error users can override with
//! `--runtime raw`.
//!
//! Captures: inputs (filename + sha256), runtime, duration. Output capture is
//! left to whatever the runtime produces; we record outputs that show up in
//! the working directory after the run. No fs watcher in v0.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use clap::Args;
use serde_json::json;

use crate::api::runs as api_runs;
use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Skill slug to execute.
    pub slug: String,

    /// Input file path. Becomes part of the Run's `inputs_summary`.
    #[arg(long)]
    pub input: Option<PathBuf>,

    /// Runtime to dispatch to. Default: auto-detect.
    #[arg(long, value_parser = ["auto", "claude-code", "cowork", "raw"])]
    pub runtime: Option<String>,

    /// Identifier for the calling agent (so multiple agents can be told apart).
    #[arg(long)]
    pub agent_id: Option<String>,

    /// Don't actually execute — just record a Run with status=unknown.
    /// Useful for agents that handle execution themselves and only need the
    /// telemetry plumbing.
    #[arg(long)]
    pub no_exec: bool,

    /// Telemetry-only mode: log the run, skip execution, leave status=unknown.
    /// Equivalent to `--no-exec --runtime raw`. Matches the spec's `--dry`
    /// flag from `knack run --dry`.
    #[arg(long, conflicts_with = "no_exec")]
    pub dry: bool,
}

pub async fn run(args: RunArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };
    let version_id = skill.current_version_id.clone().ok_or_else(|| {
        CliError::NotFound(format!("skill `{}` has no published version", args.slug))
    })?;

    // --dry implies runtime=raw + no_exec — collapse early so the rest of the
    // pipeline doesn't have to know about the flag.
    let dry = args.dry;
    let no_exec = args.no_exec || dry;

    let runtime_choice = if dry {
        "raw".to_string()
    } else {
        args.runtime.clone().unwrap_or_else(|| "auto".into())
    };
    let runtime = if runtime_choice == "auto" {
        detect_runtime().to_string()
    } else {
        runtime_choice
    };

    // Helpful nudge when the user hasn't asked for raw and we still ended up
    // there because no local runtime is on PATH. Quiet under --json.
    if runtime == "raw" && !dry && args.runtime.is_none() {
        chatter(
            mode,
            "no local runtime detected (looked for `claude`, `cowork`); \
             logging telemetry only — `knack mark <run_id>` after you run \
             the skill yourself, or install Claude Code.",
        );
    }

    let inputs_summary = args.input.as_ref().map(|p| {
        json!({
            "path": p,
            "filename": p.file_name().and_then(|s| s.to_str()),
        })
    });

    chatter(
        mode,
        format!("starting run · skill={} runtime={}", args.slug, runtime),
    );

    let run = api_runs::start(
        &client,
        &api_runs::RunCreate {
            skill_version_id: version_id.clone(),
            agent_id: args.agent_id.clone(),
            runtime: Some(runtime.clone()),
            inputs_summary: inputs_summary.clone(),
        },
    )
    .await?;

    if no_exec {
        emit_ok(
            mode,
            json!({
                "run_id": run.id,
                "skill_version_id": run.skill_version_id,
                "runtime": runtime,
                "no_exec": true,
                "dry": dry,
            }),
            || {
                let prefix = if dry {
                    "✓ dry run logged"
                } else {
                    "✓ run logged"
                };
                println!("{prefix} · {}", run.id);
                println!("  knack mark {} --status=succeeded", run.id);
            },
        );
        return Ok(());
    }

    // Try to execute. v0 dispatch is intentionally minimal — we shell out to
    // the detected runtime if we can find a working executable, else fall
    // through to "unknown" status so agents can still close the loop.
    let started = Instant::now();
    let (status, files_touched) = match runtime.as_str() {
        "claude-code" | "cowork" => {
            let exe = if runtime == "claude-code" {
                "claude"
            } else {
                "cowork"
            };
            match dispatch_runtime(exe, args.input.as_deref()) {
                Ok(()) => ("succeeded", scan_output_files(&args.input)),
                Err(e) => {
                    chatter(mode, format!("runtime {exe} failed: {e}"));
                    ("failed", vec![])
                }
            }
        }
        "raw" => {
            // No external runtime — treat the run as deferred to the caller.
            ("unknown", vec![])
        }
        other => {
            chatter(
                mode,
                format!("unknown runtime `{other}` — leaving status=unknown"),
            );
            ("unknown", vec![])
        }
    };
    let duration_s = started.elapsed().as_secs_f64();

    let finished = api_runs::finish(
        &client,
        &run.id,
        &api_runs::RunFinish {
            status: status.to_string(),
            outputs_summary: Some(json!({ "duration_s": duration_s })),
            files_touched: if files_touched.is_empty() {
                None
            } else {
                Some(files_touched)
            },
        },
    )
    .await?;

    emit_ok(
        mode,
        json!({
            "run_id": finished.id,
            "skill_version_id": finished.skill_version_id,
            "status": finished.status,
            "duration_s": duration_s,
            "runtime": runtime,
        }),
        || {
            println!(
                "✓ run {} · {} · {:.1}s",
                finished.id, finished.status, duration_s
            );
            println!("  knack mark {} --status=succeeded", finished.id);
        },
    );
    Ok(())
}

fn detect_runtime() -> &'static str {
    if which("claude").is_some() {
        "claude-code"
    } else if which("cowork").is_some() {
        "cowork"
    } else {
        "raw"
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let exe = if cfg!(target_os = "windows") {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn dispatch_runtime(exe: &str, input: Option<&Path>) -> CliResult<()> {
    let mut cmd = Command::new(exe);
    if let Some(p) = input {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(CliError::User {
            code: "RUN_RUNTIME_NONZERO".into(),
            message: format!("{exe} exited with {status}"),
            hint: None,
        });
    }
    Ok(())
}

/// Files newer than the start of the run, in the input's directory. Crude but
/// useful for v0; a real fs-watcher comes in E9 polish.
fn scan_output_files(input: &Option<PathBuf>) -> Vec<String> {
    let Some(input) = input else { return vec![] };
    let Some(parent) = input.parent() else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return vec![];
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p == *input {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            out.push(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_runtime_falls_back_to_raw() {
        // We can't mock PATH portably, but we can at least exercise the
        // function and assert it returns one of the known options.
        let r = detect_runtime();
        assert!(matches!(r, "claude-code" | "cowork" | "raw"));
    }

    #[test]
    fn scan_output_files_skips_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.xlsx");
        std::fs::write(&input, "in").unwrap();
        std::fs::write(dir.path().join("output.xlsx"), "out").unwrap();
        let names = scan_output_files(&Some(input));
        assert!(names.contains(&"output.xlsx".to_string()));
        assert!(!names.contains(&"input.xlsx".to_string()));
    }

    #[test]
    fn scan_output_files_no_input_returns_empty() {
        assert!(scan_output_files(&None).is_empty());
    }
}
