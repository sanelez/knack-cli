//! `knack install` — register knack with the AI agent on this machine.
//!
//! Appends a small "use knack for skill management" block to the agent's
//! persistent context file (CLAUDE.md, AGENTS.md, .cursor/rules/knack.mdc,
//! etc.) so the agent picks knack up automatically on its next turn.
//!
//! Idempotent: the block is delimited by HTML-comment sentinels, so re-runs
//! splice in place without ever duplicating or clobbering surrounding user
//! content. `--uninstall` removes it cleanly.
//!
//! Autodetect (when called with no arg or `--auto`): walk the target
//! registry, take the first env-marker hit (e.g. `CLAUDECODE=1`), fall back
//! to the first binary-on-PATH hit. Always also writes the generic
//! `~/.config/agents/AGENTS.md` as a safety net so a future AGENTS.md-aware
//! agent picks knack up without re-installing.

pub mod block;
pub mod detect;
pub mod targets;

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

use targets::{AgentTarget, ConfigStyle};

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Specific agent slug ("claude", "codex", "cursor", "aider", "gemini",
    /// "opencode", "factory", "amp", "generic"). Omit for autodetect.
    pub target: Option<String>,

    /// Same as bare `knack install`. Exists so installer scripts can be
    /// explicit ("we mean autodetect, not 'please ask the user'").
    #[arg(long)]
    pub auto: bool,

    /// Write to every known target on this machine. Useful for testing.
    #[arg(long, conflicts_with = "target")]
    pub all: bool,

    /// Print what would be written without touching the filesystem.
    #[arg(long)]
    pub print: bool,

    /// Remove the knack block from every known agent config file.
    #[arg(long, conflicts_with_all = ["target", "all", "auto", "print"])]
    pub uninstall: bool,
}

/// Rendered body for an agent target, including any frontmatter the target's
/// style requires.
const SNIPPET: &str = include_str!("snippet.md");

pub fn run(args: InstallArgs, mode: OutputMode) -> CliResult<()> {
    if args.uninstall {
        return run_uninstall(mode);
    }

    let targets = select_targets(&args, mode)?;
    let report = apply(&targets, args.print);
    emit_report(mode, &report, args.print);
    Ok(())
}

fn select_targets(
    args: &InstallArgs,
    mode: OutputMode,
) -> CliResult<Vec<&'static AgentTarget>> {
    if args.all {
        return Ok(targets::TARGETS.iter().collect());
    }
    if let Some(name) = &args.target {
        return match targets::find(name) {
            Some(t) => Ok(vec![t]),
            None => {
                let err = CliError::User {
                    code: "UNKNOWN_TARGET".into(),
                    message: format!("unknown agent target: {name}"),
                    hint: Some(format!("known: {}", targets::names().join(", "))),
                };
                emit_err(mode, &err);
                Err(err)
            }
        };
    }
    // Autodetect path: first agent-specific hit, plus generic as safety net.
    let mut out: Vec<&'static AgentTarget> = Vec::new();
    if let Some(t) = detect::autodetect() {
        out.push(t);
    }
    if let Some(g) = targets::find("generic") {
        if !out.iter().any(|t| t.name == "generic") {
            out.push(g);
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct TargetOutcome {
    name: &'static str,
    display: &'static str,
    path: Option<PathBuf>,
    body: String,
    status: Status,
}

#[derive(Debug)]
enum Status {
    Wrote,
    UpToDate,
    DryRun,
    Skipped(String),
}

fn apply(targets: &[&'static AgentTarget], dry_run: bool) -> Vec<TargetOutcome> {
    targets
        .iter()
        .map(|t| {
            let body = render_body(t);
            let path = (t.config_path)();
            let Some(path) = path else {
                return TargetOutcome {
                    name: t.name,
                    display: t.display,
                    path: None,
                    body,
                    status: Status::Skipped("no config path on this OS".into()),
                };
            };
            if dry_run {
                return TargetOutcome {
                    name: t.name,
                    display: t.display,
                    path: Some(path),
                    body,
                    status: Status::DryRun,
                };
            }
            let res = match t.style {
                ConfigStyle::AppendBlock => block::upsert(&path, &body),
                ConfigStyle::WriteFile => write_full(&path, &body),
            };
            let status = match res {
                Ok(true) => Status::Wrote,
                Ok(false) => Status::UpToDate,
                Err(e) => Status::Skipped(format!("write failed: {e}")),
            };
            TargetOutcome {
                name: t.name,
                display: t.display,
                path: Some(path),
                body,
                status,
            }
        })
        .collect()
}

fn run_uninstall(mode: OutputMode) -> CliResult<()> {
    let mut removed: Vec<(&'static str, String)> = Vec::new();
    for t in targets::TARGETS {
        let Some(path) = (t.config_path)() else {
            continue;
        };
        let did = match t.style {
            ConfigStyle::WriteFile => {
                if path.exists() && std::fs::remove_file(&path).is_ok() {
                    true
                } else {
                    false
                }
            }
            ConfigStyle::AppendBlock => block::remove(&path).unwrap_or(false),
        };
        if did {
            removed.push((t.name, path.display().to_string()));
        }
    }
    if mode.json {
        emit_ok(
            mode,
            json!({
                "removed": removed.iter().map(|(n, p)| json!({ "target": n, "path": p })).collect::<Vec<_>>(),
            }),
            || {},
        );
    } else if removed.is_empty() {
        println!("Nothing to uninstall (no knack block found in any known target).");
    } else {
        for (n, p) in &removed {
            println!("Removed {n} → {p}");
        }
    }
    Ok(())
}

fn write_full(path: &std::path::Path, body: &str) -> std::io::Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if existing == body {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    Ok(true)
}

fn render_body(target: &AgentTarget) -> String {
    match target.style {
        ConfigStyle::WriteFile => format!(
            "---\ndescription: Use the knack CLI for portable skill management\nalwaysApply: true\n---\n\n{}",
            SNIPPET.trim_end_matches('\n')
        ),
        ConfigStyle::AppendBlock => SNIPPET.trim_end_matches('\n').to_string(),
    }
}

fn emit_report(mode: OutputMode, outcomes: &[TargetOutcome], dry_run: bool) {
    if mode.json {
        let items: Vec<_> = outcomes
            .iter()
            .map(|o| {
                let (status, reason) = match &o.status {
                    Status::Wrote => ("wrote", None),
                    Status::UpToDate => ("up_to_date", None),
                    Status::DryRun => ("dry_run", None),
                    Status::Skipped(r) => ("skipped", Some(r.clone())),
                };
                json!({
                    "target": o.name,
                    "display": o.display,
                    "path": o.path.as_ref().map(|p| p.display().to_string()),
                    "status": status,
                    "reason": reason,
                })
            })
            .collect();
        emit_ok(
            mode,
            json!({
                "dry_run": dry_run,
                "targets": items,
            }),
            || {},
        );
        return;
    }
    if mode.quiet {
        return;
    }
    for o in outcomes {
        match &o.status {
            Status::Wrote => println!(
                "Wrote {} → {}",
                o.display,
                o.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            Status::UpToDate => println!("Already up to date: {}", o.display),
            Status::DryRun => {
                println!(
                    "[dry-run] Would write {} → {}",
                    o.display,
                    o.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
                );
                println!("--- begin block ---");
                println!("{}", o.body);
                println!("--- end block ---");
            }
            Status::Skipped(reason) => println!("Skipped {}: {}", o.display, reason),
        }
    }
}
