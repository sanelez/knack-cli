//! `knack sync` — keep per-skill shims in step with `.knack/skills/`.
//!
//! For every agent recorded in `installed.json` (or detected with
//! `--all-detected`), walk the workspace skills directory and write /
//! refresh / prune the runtime's native discovery shims so the agent
//! picks up Knack-managed skills on its next session start.
//!
//! Called explicitly via `knack sync`, *and* implicitly after every
//! successful `knack pull` / `knack publish` so the agent doesn't need
//! a separate step.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};
use crate::skill_pack::{parse_skill_md_frontmatter, SkillFrontmatter};
use crate::workspace::{discover_workspace_root, SKILLS_SUBDIR};

use super::install::{
    detect, installed,
    installed::{AgentEntry, Scope},
    shim,
    targets::{self, AgentTarget, ShimStyle},
};

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Limit sync to one runtime by slug ("claude", "cursor", ...).
    #[arg(long)]
    pub agent: Option<String>,

    /// Bypass the `installed.json` record and union it with the result of
    /// `detect::list_installed()`. Useful right after the user installs
    /// a new agent on the machine without re-running `knack install`.
    #[arg(long)]
    pub all_detected: bool,

    /// Print intended writes/deletes; touch nothing on disk.
    #[arg(long)]
    pub dry_run: bool,

    /// Remove every knack-authored shim across all known agents. Pairs
    /// nicely with an eventual `knack install --uninstall`.
    #[arg(long, conflicts_with_all = ["agent", "all_detected"])]
    pub purge: bool,

    /// Sync the HOME-scoped pool (`~/.knack/skills/`) instead of the
    /// workspace-local `.knack/skills/`. Pairs with `knack pull --global`.
    #[arg(long)]
    pub global: bool,
}

// ─── public entry ──────────────────────────────────────────────────────────

pub fn run(args: SyncArgs, client_config: Config, mode: OutputMode) -> CliResult<()> {
    if args.purge {
        let report = purge_all(args.dry_run);
        emit_report(mode, &report, args.dry_run);
        return Ok(());
    }

    let targets = match resolve_targets(&args, mode) {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    if targets.is_empty() {
        chatter(
            mode,
            "no installed agents to sync. run `knack install` first, or pass --all-detected.",
        );
        emit_ok(
            mode,
            json!({"written": [], "up_to_date": [], "removed": [], "skipped": []}),
            || {},
        );
        return Ok(());
    }

    let scope = if args.global { Scope::Home } else { Scope::Project };
    let skills_root = resolve_skills_root(scope, &client_config);
    let skills = list_skills(&skills_root);

    let report = sync_targets(&targets, scope, &skills, args.dry_run);
    emit_report(mode, &report, args.dry_run);
    Ok(())
}

/// Sync a single skill after a `knack pull` / `knack publish`. Returns
/// the per-target outcomes for inclusion in the parent command's JSON
/// emit (e.g. `pull` surfaces `shims: [...]` in its result).
///
/// Best-effort: per-target failures are caught and folded into the
/// `Skipped` arm so the caller's overall exit stays clean.
pub fn sync_one_skill(slug: &str, scope: Scope, client_config: &Config) -> SyncReport {
    let entries = match resolved_entries_for_scope(scope) {
        Ok(v) => v,
        Err(e) => {
            return SyncReport::from_skip(format!("could not load installed.json: {e}"));
        }
    };
    if entries.is_empty() {
        return SyncReport::default();
    }

    let skills_root = resolve_skills_root(scope, client_config);
    let skill_dir = skills_root.join(slug);
    if !skill_dir.is_dir() {
        return SyncReport::from_skip(format!(
            "skill folder not found at {}",
            skill_dir.display()
        ));
    }

    let mut report = SyncReport::default();
    for entry in entries {
        let target = match targets::find(&entry.slug) {
            Some(t) => t,
            None => continue,
        };
        match write_one(target, &skill_dir, slug, scope, false) {
            Ok(WriteOutcome::Wrote { path }) => {
                report.written.push(ShimResult {
                    agent: entry.slug.clone(),
                    path: path.display().to_string(),
                    status: "written".into(),
                    reason: None,
                });
            }
            Ok(WriteOutcome::UpToDate { path }) => {
                report.up_to_date.push(ShimResult {
                    agent: entry.slug.clone(),
                    path: path.display().to_string(),
                    status: "up_to_date".into(),
                    reason: None,
                });
            }
            Ok(WriteOutcome::Skipped { reason, path }) => {
                report.skipped.push(ShimResult {
                    agent: entry.slug.clone(),
                    path: path.map(|p| p.display().to_string()).unwrap_or_default(),
                    status: "skipped".into(),
                    reason: Some(reason),
                });
            }
            Err(e) => {
                report.skipped.push(ShimResult {
                    agent: entry.slug.clone(),
                    path: String::new(),
                    status: "skipped".into(),
                    reason: Some(e.to_string()),
                });
            }
        }
    }
    report
}

// ─── core sync ─────────────────────────────────────────────────────────────

fn sync_targets(
    targets: &[&'static AgentTarget],
    scope: Scope,
    skills: &[String],
    dry_run: bool,
) -> SyncReport {
    let mut report = SyncReport::default();

    for target in targets {
        let Some(root) = (target.shim_root)(scope) else {
            continue;
        };
        // Reconcile: write/refresh each current skill, then prune stale.
        for slug in skills {
            // Locate the source SKILL.md.
            let canonical_dir = resolve_canonical_skill_dir(scope, slug);
            let outcome = if dry_run {
                WriteOutcome::Wrote {
                    path: shim_target_path(target, &root, slug),
                }
            } else {
                match write_one(target, &canonical_dir, slug, scope, false) {
                    Ok(o) => o,
                    Err(e) => WriteOutcome::Skipped {
                        reason: e.to_string(),
                        path: Some(shim_target_path(target, &root, slug)),
                    },
                }
            };
            push_outcome(&mut report, target, outcome);
        }

        // Stale-shim cleanup.
        let stale = enumerate_stale_shims(target, &root, skills);
        for slug in stale {
            if dry_run {
                report.removed.push(ShimResult {
                    agent: target.name.to_string(),
                    path: shim_target_path(target, &root, &slug).display().to_string(),
                    status: "would_remove".into(),
                    reason: None,
                });
                continue;
            }
            match remove_one(target, &root, &slug) {
                Ok(true) => report.removed.push(ShimResult {
                    agent: target.name.to_string(),
                    path: shim_target_path(target, &root, &slug).display().to_string(),
                    status: "removed".into(),
                    reason: None,
                }),
                Ok(false) => {}
                Err(e) => report.skipped.push(ShimResult {
                    agent: target.name.to_string(),
                    path: shim_target_path(target, &root, &slug).display().to_string(),
                    status: "skipped".into(),
                    reason: Some(e.to_string()),
                }),
            }
        }
    }

    report
}

fn purge_all(dry_run: bool) -> SyncReport {
    let mut report = SyncReport::default();
    for entry in installed::list().unwrap_or_default() {
        let Some(target) = targets::find(&entry.slug) else { continue };
        let Some(root) = (target.shim_root)(entry.scope) else { continue };
        if dry_run {
            report.removed.push(ShimResult {
                agent: entry.slug.clone(),
                path: root.display().to_string(),
                status: "would_purge".into(),
                reason: None,
            });
            continue;
        }
        match shim::remove_all_shims(target, &root) {
            Ok(n) => {
                if n > 0 {
                    report.removed.push(ShimResult {
                        agent: entry.slug.clone(),
                        path: root.display().to_string(),
                        status: format!("purged_{n}"),
                        reason: None,
                    });
                }
            }
            Err(e) => report.skipped.push(ShimResult {
                agent: entry.slug,
                path: root.display().to_string(),
                status: "skipped".into(),
                reason: Some(e.to_string()),
            }),
        }
    }
    report
}

// ─── per-target write + remove ─────────────────────────────────────────────

enum WriteOutcome {
    Wrote { path: PathBuf },
    UpToDate { path: PathBuf },
    Skipped { reason: String, path: Option<PathBuf> },
}

/// Render + write a single skill's shim for one target. The shim's
/// destination is determined by `scope` (the pull / sync scope), not
/// by where the agent's install block lives.
fn write_one(
    target: &'static AgentTarget,
    canonical_dir: &Path,
    slug: &str,
    scope: Scope,
    _dry_run: bool,
) -> Result<WriteOutcome, CliError> {
    let canonical_md = canonical_dir.join("SKILL.md");
    if !canonical_md.is_file() {
        return Ok(WriteOutcome::Skipped {
            reason: format!("canonical SKILL.md missing at {}", canonical_md.display()),
            path: None,
        });
    }
    let body_src = fs::read_to_string(&canonical_md)
        .map_err(|e| CliError::Internal(format!("read canonical SKILL.md: {e}")))?;
    let fm = parse_skill_md_frontmatter(&body_src)
        .unwrap_or(None)
        .unwrap_or(SkillFrontmatter::default());

    match target.shim_style {
        ShimStyle::NativeSkill => {
            let Some(root) = (target.shim_root)(scope) else {
                return Ok(WriteOutcome::Skipped {
                    reason: "no shim root for this scope".into(),
                    path: None,
                });
            };
            let body = shim::render_native_skill(slug, &fm, &canonical_md);
            let path = root.join(slug).join("SKILL.md");
            let changed = shim::write_native_skill(&root, slug, &body)
                .map_err(|e| CliError::Internal(format!("write claude shim: {e}")))?;
            Ok(if changed {
                WriteOutcome::Wrote { path }
            } else {
                WriteOutcome::UpToDate { path }
            })
        }
        ShimStyle::NativeRule => {
            let Some(root) = (target.shim_root)(scope) else {
                return Ok(WriteOutcome::Skipped {
                    reason: "no shim root for this scope".into(),
                    path: None,
                });
            };
            let body = shim::render_native_rule(slug, &fm, &canonical_md);
            let path = root.join(format!("knack-{slug}.mdc"));
            let changed = shim::write_native_rule(&root, slug, &body)
                .map_err(|e| CliError::Internal(format!("write cursor shim: {e}")))?;
            Ok(if changed {
                WriteOutcome::Wrote { path }
            } else {
                WriteOutcome::UpToDate { path }
            })
        }
        ShimStyle::TextBlock => {
            let Some(file) = (target.shim_root)(scope) else {
                return Ok(WriteOutcome::Skipped {
                    reason: "no shim file for this scope".into(),
                    path: None,
                });
            };
            let body = shim::render_text_block(slug, &fm);
            let changed = shim::upsert_skill_block(&file, slug, &body)
                .map_err(|e| CliError::Internal(format!("write text-block shim: {e}")))?;
            Ok(if changed {
                WriteOutcome::Wrote { path: file }
            } else {
                WriteOutcome::UpToDate { path: file }
            })
        }
        ShimStyle::None => Ok(WriteOutcome::Skipped {
            reason: "target has no shim style".into(),
            path: None,
        }),
    }
}

fn remove_one(
    target: &'static AgentTarget,
    root: &Path,
    slug: &str,
) -> std::io::Result<bool> {
    match target.shim_style {
        ShimStyle::NativeSkill => shim::remove_native_skill(root, slug),
        ShimStyle::NativeRule => shim::remove_native_rule(root, slug),
        ShimStyle::TextBlock => shim::remove_skill_block(root, slug),
        ShimStyle::None => Ok(false),
    }
}

/// Where the shim file for `target`/`slug` will land. Used for logging
/// and dry-run reports; doesn't perform any filesystem work.
fn shim_target_path(target: &AgentTarget, root: &Path, slug: &str) -> PathBuf {
    match target.shim_style {
        ShimStyle::NativeSkill => root.join(slug).join("SKILL.md"),
        ShimStyle::NativeRule => root.join(format!("knack-{slug}.mdc")),
        ShimStyle::TextBlock => root.to_path_buf(),
        ShimStyle::None => root.to_path_buf(),
    }
}

/// Walk the runtime's shim root and return slugs whose canonical
/// `.knack/skills/<slug>/` no longer exists. Sigil-protected — files
/// without our SHIM_SIGIL are invisible to this enumeration.
fn enumerate_stale_shims(
    target: &AgentTarget,
    root: &Path,
    current_slugs: &[String],
) -> Vec<String> {
    let mut stale = Vec::new();
    match target.shim_style {
        ShimStyle::NativeSkill => {
            let Ok(rd) = fs::read_dir(root) else { return stale };
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() { continue; }
                let skill_md = p.join("SKILL.md");
                if !skill_md.is_file() { continue; }
                if !carries_sigil(&skill_md) { continue; }
                let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
                if !current_slugs.iter().any(|s| s == name) {
                    stale.push(name.to_string());
                }
            }
        }
        ShimStyle::NativeRule => {
            let Ok(rd) = fs::read_dir(root) else { return stale };
            for entry in rd.flatten() {
                let p = entry.path();
                let Some(fname) = p.file_name().and_then(|s| s.to_str()) else { continue };
                let Some(slug) = fname
                    .strip_prefix("knack-")
                    .and_then(|s| s.strip_suffix(".mdc"))
                else {
                    continue;
                };
                if !carries_sigil(&p) { continue; }
                if !current_slugs.iter().any(|s| s == slug) {
                    stale.push(slug.to_string());
                }
            }
        }
        ShimStyle::TextBlock => {
            // Find every <!-- knack:skill:<slug>:start --> block in the
            // context file, mark stale ones for removal.
            let Ok(body) = fs::read_to_string(root) else { return stale };
            let prefix = "<!-- knack:skill:";
            let mut cur = body.as_str();
            while let Some(idx) = cur.find(prefix) {
                let rest = &cur[idx + prefix.len()..];
                let Some(colon) = rest.find(':') else { break };
                let slug = &rest[..colon];
                if !current_slugs.iter().any(|s| s == slug) {
                    stale.push(slug.to_string());
                }
                // Advance past this start marker to keep scanning.
                cur = &rest[colon..];
            }
        }
        ShimStyle::None => {}
    }
    stale
}

fn carries_sigil(path: &Path) -> bool {
    let Ok(s) = fs::read_to_string(path) else { return false };
    s.lines().next().map(|l| l.trim() == shim::SHIM_SIGIL).unwrap_or(false)
}

// ─── helpers + types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ShimResult {
    pub agent: String,
    pub path: String,
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncReport {
    pub written: Vec<ShimResult>,
    pub up_to_date: Vec<ShimResult>,
    pub removed: Vec<ShimResult>,
    pub skipped: Vec<ShimResult>,
}

impl SyncReport {
    fn from_skip(reason: String) -> Self {
        Self {
            written: Vec::new(),
            up_to_date: Vec::new(),
            removed: Vec::new(),
            skipped: vec![ShimResult {
                agent: String::new(),
                path: String::new(),
                status: "skipped".into(),
                reason: Some(reason),
            }],
        }
    }
}

fn push_outcome(report: &mut SyncReport, target: &AgentTarget, outcome: WriteOutcome) {
    match outcome {
        WriteOutcome::Wrote { path } => report.written.push(ShimResult {
            agent: target.name.into(),
            path: path.display().to_string(),
            status: "written".into(),
            reason: None,
        }),
        WriteOutcome::UpToDate { path } => report.up_to_date.push(ShimResult {
            agent: target.name.into(),
            path: path.display().to_string(),
            status: "up_to_date".into(),
            reason: None,
        }),
        WriteOutcome::Skipped { reason, path } => report.skipped.push(ShimResult {
            agent: target.name.into(),
            path: path.map(|p| p.display().to_string()).unwrap_or_default(),
            status: "skipped".into(),
            reason: Some(reason),
        }),
    }
}

fn resolve_targets(
    args: &SyncArgs,
    mode: OutputMode,
) -> CliResult<Vec<&'static AgentTarget>> {
    if let Some(slug) = &args.agent {
        return match targets::find(slug) {
            Some(t) => Ok(vec![t]),
            None => {
                let err = CliError::User {
                    code: "UNKNOWN_TARGET".into(),
                    message: format!("unknown agent: {slug}"),
                    hint: Some(format!("known: {}", targets::names().join(", "))),
                };
                emit_err(mode, &err);
                Err(err)
            }
        };
    }
    let recorded: Vec<&'static AgentTarget> = installed::list()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| targets::find(&e.slug))
        .collect();
    if !args.all_detected {
        return Ok(dedupe(recorded));
    }
    let mut combined = recorded;
    for t in detect::list_installed() {
        if !combined.iter().any(|c| c.name == t.name) {
            combined.push(t);
        }
    }
    Ok(dedupe(combined))
}

fn dedupe(mut v: Vec<&'static AgentTarget>) -> Vec<&'static AgentTarget> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|t| seen.insert(t.name));
    v
}

fn resolved_entries_for_scope(scope: Scope) -> std::io::Result<Vec<AgentEntry>> {
    let all = installed::list()?;
    Ok(all
        .into_iter()
        .filter(|e| matches_scope(e, scope))
        .collect())
}

/// Whether an installed-agent entry should receive a shim for a pull at
/// the given scope.
///
/// The install record's `scope` field tracks where the install BLOCK
/// landed (HOME-anchored config path vs workspace-anchored). It does
/// NOT constrain where per-skill shims go: the *pull* scope determines
/// the shim's destination via the target's `shim_root(pull_scope)`.
/// That decoupling is what lets a globally installed Knack still write
/// a workspace-scoped per-skill shim when the user pulls in a repo —
/// the shim lands in `<repo>/.claude/skills/`, never bleeding into
/// other projects, while the install block at `~/.claude/CLAUDE.md`
/// stays untouched.
///
/// So `matches_scope` is just "is this agent registered and capable of
/// receiving shims at all?" — scope matching is irrelevant.
fn matches_scope(entry: &AgentEntry, _pull_scope: Scope) -> bool {
    let Some(t) = targets::find(&entry.slug) else {
        return false;
    };
    !matches!(t.shim_style, ShimStyle::None)
}

fn resolve_skills_root(scope: Scope, client_config: &Config) -> PathBuf {
    match scope {
        Scope::Home => client_config.skills_dir.clone(),
        Scope::Project => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if let Some(ws) = discover_workspace_root(&cwd) {
                return ws.join(SKILLS_SUBDIR);
            }
            cwd.join(".knack").join(SKILLS_SUBDIR)
        }
    }
}

fn resolve_canonical_skill_dir(scope: Scope, slug: &str) -> PathBuf {
    let root = resolve_skills_root(scope, &Config::load());
    root.join(slug)
}

fn list_skills(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else { return out };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("SKILL.md").is_file() {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

fn emit_report(mode: OutputMode, report: &SyncReport, dry_run: bool) {
    if mode.json {
        emit_ok(
            mode,
            json!({
                "dry_run": dry_run,
                "written": report.written,
                "up_to_date": report.up_to_date,
                "removed": report.removed,
                "skipped": report.skipped,
            }),
            || {},
        );
        return;
    }
    if mode.quiet {
        return;
    }
    if dry_run {
        println!("[dry-run]");
    }
    for r in &report.written {
        println!("Wrote   {:<10} → {}", r.agent, r.path);
    }
    for r in &report.up_to_date {
        println!("Skipped {:<10} (up to date) {}", r.agent, r.path);
    }
    for r in &report.removed {
        println!("Removed {:<10} {}", r.agent, r.path);
    }
    for r in &report.skipped {
        println!(
            "Skipped {:<10} ({}) {}",
            r.agent,
            r.reason.as_deref().unwrap_or("?"),
            r.path
        );
    }
    if report.written.is_empty()
        && report.up_to_date.is_empty()
        && report.removed.is_empty()
        && report.skipped.is_empty()
    {
        println!("Nothing to sync.");
    }
}
