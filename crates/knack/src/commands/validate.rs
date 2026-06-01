//! `knack validate <dir>` — local pre-flight check before publishing.
//!
//! Runs the same shape checks the server's `SKILL_FORMAT_INVALID` path
//! enforces, so the user catches missing required fields (most commonly an
//! incomplete `meta.knack.yaml`) without paying a round-trip. Output
//! envelope shape matches the server's `VALIDATION_ERROR` so agents that
//! already handle the remote case need no code change.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::errors::CliResult;
use crate::output::{emit_ok, OutputMode};
use crate::skill_validators::{emit_format_invalid, validate_skill_folder};

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Skill folder to inspect. Must contain SKILL.md and meta.knack.yaml.
    pub dir: PathBuf,
}

pub fn run(args: ValidateArgs, mode: OutputMode) -> CliResult<()> {
    let report = validate_skill_folder(&args.dir);
    if report.is_ok() {
        emit_ok(
            mode,
            json!({
                "dir": args.dir.display().to_string(),
                "ok": true,
            }),
            || println!("✓ {} validates cleanly", args.dir.display()),
        );
        return Ok(());
    }
    Err(emit_format_invalid(mode, report))
}
