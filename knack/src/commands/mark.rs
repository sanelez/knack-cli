//! `knack mark <run_id> --status=… [--note=…]` — close the agent loop.

use clap::Args;
use serde_json::json;

use crate::api::{runs as api_runs, ApiClient};
use crate::errors::CliResult;
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Run id (UUID) — `knack run` prints it on every invocation.
    pub run_id: String,

    /// Outcome.
    #[arg(long, value_parser = ["succeeded", "failed"])]
    pub status: String,

    /// Free-form note. For `--status=failed`, the skill author gets notified
    /// with this text — be specific.
    #[arg(long)]
    pub note: Option<String>,

    /// Alias for `--note`. Spec uses `--reason` for failures.
    #[arg(long, conflicts_with = "note")]
    pub reason: Option<String>,
}

pub async fn run(args: MarkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let note = args.note.or(args.reason);
    let result = api_runs::mark(
        &client,
        &args.run_id,
        &api_runs::RunMarkBody {
            status: args.status.clone(),
            note: note.clone(),
        },
    )
    .await;

    match result {
        Ok(run) => {
            emit_ok(
                mode,
                json!({
                    "run_id": run.id,
                    "status": args.status,
                    "note": note,
                    "marks_count": run.marks.len(),
                }),
                || println!("✓ marked {} {}", run.id, args.status),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
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
