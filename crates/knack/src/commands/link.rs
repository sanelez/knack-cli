//! `knack link <slug>` / `knack unlink <slug>` — install a published skill
//! as a native slash command in every agent on this machine, while
//! preserving knack's telemetry loop.
//!
//! Unlike `knack pull` (which drops a skill into `.knack/skills/`, a
//! directory no agent scans), `link` writes the *whole* bundle into each
//! installed agent's native skill directory — `~/.claude/skills/<slug>/`,
//! `~/.agents/skills/<slug>/` (Codex), etc. — so the skill shows up as a
//! real `/<slug>` command. The written `SKILL.md` keeps its original
//! frontmatter verbatim and gains an injected telemetry wrapper: the agent
//! records a run with `knack run` before doing the work and closes it with
//! `knack mark` after, so linked skills still feed run telemetry. Telemetry
//! is best-effort — if `knack run` fails (offline / signed out) the wrapper
//! tells the agent to proceed anyway, so a linked skill is never blocked.
//!
//! Scope: `--global` writes to the HOME-shared skill dirs (every project
//! sees the command); `--local` writes to the workspace `.<agent>/skills/`
//! (this project only). With neither flag, the default comes from
//! `config.link_scope` (`defaults.link_scope` in `~/.knack/config.yaml`,
//! itself defaulting to `home`).
//!
//! Extensible by construction: the per-agent destinations and write styles
//! all come from [`crate::commands::install::targets`], so any agent added
//! to that registry is linkable with no changes here.

use std::collections::HashSet;
use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::{BackendMode, LinkScope};
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};
use crate::skill_pack::{parse_skill_md_frontmatter, unpack_skill, SkillFrontmatter};

use crate::commands::install::installed::{self, Scope};
use crate::commands::install::{linked, shim, targets};
use targets::{AgentTarget, ShimStyle};

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Skill identifier — `<slug>` or `<slug>@<semver>`. Optional only
    /// when `--list` is passed.
    pub slug_at_version: Option<String>,

    /// Install into the HOME-shared agent skill dirs (`~/.claude/skills/…`)
    /// so the `/<slug>` command works in every project. This is the
    /// built-in default; the flag is here to override a `project` config
    /// default.
    #[arg(long)]
    pub global: bool,

    /// Install into the workspace agent skill dirs (`.claude/skills/…`)
    /// so the command exists only in this project.
    #[arg(long, conflicts_with = "global")]
    pub local: bool,

    /// Link into one specific agent (by target slug, e.g. `claude`,
    /// `codex`, `cursor`) instead of every installed agent.
    #[arg(long)]
    pub agent: Option<String>,

    /// Show what would be written without touching the filesystem.
    #[arg(long)]
    pub print: bool,

    /// List skills currently linked on this machine and exit.
    #[arg(long, conflicts_with_all = ["global", "local", "agent", "print"])]
    pub list: bool,

    /// Report which linked skills have a newer version published upstream
    /// (and who authored it), without changing anything. The team-workflow
    /// "what changed upstream?" view.
    #[arg(long, conflicts_with_all = ["agent", "print", "list", "global", "local", "all"])]
    pub check: bool,

    /// Explicitly update every linked skill to its latest published version
    /// (a user-initiated pull, not automatic). Re-downloads and overwrites
    /// only the copies whose version drifted; up-to-date links are untouched.
    #[arg(long, conflicts_with_all = ["agent", "print", "list", "global", "local"])]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Skill slug to remove from agent skill dirs.
    pub slug: String,

    /// Remove from the HOME-shared dirs (default scope, matching `link`).
    #[arg(long)]
    pub global: bool,

    /// Remove from the workspace dirs.
    #[arg(long, conflicts_with = "global")]
    pub local: bool,

    /// Limit removal to one agent target.
    #[arg(long)]
    pub agent: Option<String>,
}

// ─── link ────────────────────────────────────────────────────────────────────

pub async fn run_link(args: LinkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if args.list {
        return list_linked(mode);
    }
    if args.check {
        return check_updates(&client, mode).await;
    }
    if args.all {
        return update_all(&client, mode).await;
    }

    let Some(spec) = args.slug_at_version.clone() else {
        let err = CliError::User {
            code: "LINK_NO_SLUG".into(),
            message: "missing skill slug".into(),
            hint: Some(
                "usage: knack link <slug>[@<semver>]  (or `knack link --list` / `--all`)".into(),
            ),
        };
        emit_err(mode, &err);
        return Err(err);
    };

    let scope = resolve_scope(args.global, args.local, client.config.link_scope);

    let agent_targets = resolve_targets(args.agent.as_deref(), mode)?;

    // Download + unpack the skill into a temp dir so we have every file in
    // memory (the wrapped SKILL.md for NativeSkill targets, plus the
    // frontmatter description for NativeRule / TextBlock targets).
    let skill = match materialize(&client, &spec).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    let fm = parse_skill_md_frontmatter(&skill.skill_md)
        .ok()
        .flatten()
        .unwrap_or_default();

    let write = write_skill_to_targets(&skill, &fm, scope, &agent_targets, args.print);

    // Record in the registry (best-effort; bookkeeping only) and warn if a
    // same-name link in the other scope will shadow / be shadowed by this.
    let mut precedence_note: Option<String> = None;
    if !args.print && !write.written_agents.is_empty() {
        let _ = linked::add(&skill.slug, &skill.version, scope, write.written_agents.clone());
        precedence_note = precedence_warning(&skill.slug, scope);
    }

    emit_link_report(
        mode,
        &skill.slug,
        &skill.version,
        scope,
        &write.outcomes,
        args.print,
        write.created_top_level_dir,
        precedence_note.as_deref(),
    );
    Ok(())
}

/// Result of writing one materialized skill to a set of agent targets.
struct WriteResult {
    outcomes: Vec<AgentOutcome>,
    /// True when a native skills directory had to be created (the agent may
    /// need a restart to watch it).
    created_top_level_dir: bool,
    /// Target slugs the skill was actually written to.
    written_agents: Vec<String>,
}

/// Write `skill` into every `target` at `scope`, dispatching on each
/// target's shim style. Pure filesystem work — no network, no registry
/// update (callers own that). Shared by `run_link` and the auto-refresh
/// path so both produce byte-identical output.
fn write_skill_to_targets(
    skill: &Materialized,
    fm: &SkillFrontmatter,
    scope: Scope,
    targets: &[&'static AgentTarget],
    print: bool,
) -> WriteResult {
    let mut outcomes: Vec<AgentOutcome> = Vec::new();
    let mut created_top_level_dir = false;
    let mut written_agents: Vec<String> = Vec::new();

    for t in targets {
        let Some(root) = (t.shim_root)(scope) else {
            outcomes.push(AgentOutcome::skipped(
                t,
                None,
                "no skill directory for this scope on this OS",
            ));
            continue;
        };
        let root_existed = root.exists();

        match t.shim_style {
            ShimStyle::NativeSkill => {
                let dest = root.join(&skill.slug);
                if print {
                    outcomes.push(AgentOutcome::dry_run(t, dest));
                    continue;
                }
                let wrapped = shim::render_linked_skill(&skill.slug, t.name, &skill.skill_md);
                let support: Vec<(String, Vec<u8>)> = skill
                    .files
                    .iter()
                    .filter(|(rel, _)| rel != "SKILL.md")
                    .cloned()
                    .collect();
                match shim::write_linked_skill(&root, &skill.slug, &wrapped, &support) {
                    Ok(_) => {
                        if !root_existed {
                            created_top_level_dir = true;
                        }
                        written_agents.push(t.name.to_string());
                        outcomes.push(AgentOutcome::wrote(t, dest));
                    }
                    Err(e) => outcomes.push(AgentOutcome::skipped(t, Some(dest), &e.to_string())),
                }
            }
            ShimStyle::NativeRule => {
                let dest = root.join(format!("knack-{}.mdc", skill.slug));
                if print {
                    outcomes.push(AgentOutcome::dry_run(t, dest));
                    continue;
                }
                let body = shim::render_linked_rule(&skill.slug, t.name, fm, &skill.skill_md);
                match shim::write_native_rule(&root, &skill.slug, &body) {
                    Ok(_) => {
                        written_agents.push(t.name.to_string());
                        outcomes.push(AgentOutcome::wrote(t, dest));
                    }
                    Err(e) => outcomes.push(AgentOutcome::skipped(t, Some(dest), &e.to_string())),
                }
            }
            ShimStyle::TextBlock => {
                // For TextBlock, `shim_root` returns the agent's context
                // file itself; we splice a per-skill block into it.
                if print {
                    outcomes.push(AgentOutcome::dry_run(t, root.clone()));
                    continue;
                }
                let body = shim::render_linked_text_block(&skill.slug, t.name, fm);
                match shim::upsert_skill_block(&root, &skill.slug, &body) {
                    Ok(_) => {
                        written_agents.push(t.name.to_string());
                        outcomes.push(AgentOutcome::wrote(t, root.clone()));
                    }
                    Err(e) => {
                        outcomes.push(AgentOutcome::skipped(t, Some(root.clone()), &e.to_string()))
                    }
                }
            }
            ShimStyle::None => {
                outcomes.push(AgentOutcome::skipped(
                    t,
                    None,
                    "agent has no per-skill discovery surface",
                ));
            }
        }
    }

    WriteResult {
        outcomes,
        created_top_level_dir,
        written_agents,
    }
}

// ─── update notifications (notify-only; pulling is always user-initiated) ────
//
// Linked skills are PINNED to the version the user linked. We never pull a
// new remote version on our own — critical for team workflows, where a
// teammate publishing a new version must not silently change what your
// agent runs. Instead we *flag* that a newer version exists (and who the
// author is) and let the user decide to `knack link <slug>` (or
// `knack link --all`) to adopt it.

/// A pending upstream update for a linked skill.
#[derive(Debug, Clone)]
pub struct UpdateNotice {
    pub slug: String,
    /// The version currently linked on disk.
    pub have: String,
    /// The latest published version available upstream.
    pub latest: String,
    /// Human label for who owns/publishes the skill (marketplace handle, or
    /// "your team" for team-scoped skills).
    pub author: String,
}

impl UpdateNotice {
    /// One-line, neutral factual notice surfaced by `knack run`. It states
    /// the fact only and deliberately does NOT tell anyone to run a command:
    /// the linked skill's wrapper instructs the agent to OFFER to update it
    /// for the user ("want me to grab it for you?"), not to relay an
    /// imperative. Nothing is pulled automatically.
    pub fn line(&self) -> String {
        format!(
            "update available: `{}` is linked at {}; {} (by {}) is published upstream.",
            self.slug, self.have, self.latest, self.author
        )
    }
}

/// Build a human author label from a skill's ownership fields.
fn author_label(owner_username: Option<&str>, owner_team: bool) -> String {
    match owner_username {
        Some(u) if !u.is_empty() => u.to_string(),
        _ if owner_team => "your team".to_string(),
        _ => "the author".to_string(),
    }
}

/// Notify-only check used by `knack run`: if `slug` is linked and the
/// `latest` published version differs from the version recorded in the
/// link registry, return a notice. Pure and cheap — a registry read plus a
/// string compare, NO download and NO filesystem change. Returns `None`
/// when the slug isn't linked, is current, or the check is disabled via
/// `KNACK_NO_LINK_UPDATE_CHECK`.
pub fn pending_update(
    slug: &str,
    latest: &str,
    owner_username: Option<&str>,
    owner_team: bool,
) -> Option<UpdateNotice> {
    if update_check_disabled() {
        return None;
    }
    let have = linked::list()
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.slug == slug)
        .map(|s| s.version)?;
    if have == latest {
        return None;
    }
    Some(UpdateNotice {
        slug: slug.to_string(),
        have,
        latest: latest.to_string(),
        author: author_label(owner_username, owner_team),
    })
}

/// Env kill switch: set `KNACK_NO_LINK_UPDATE_CHECK=1` to silence the
/// "newer version available" flag on `knack run`.
fn update_check_disabled() -> bool {
    std::env::var("KNACK_NO_LINK_UPDATE_CHECK")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// `knack link --check`: report which linked skills have a newer version
/// upstream (and the author), WITHOUT changing anything. The explicit
/// "what did my team change?" view for team workflows.
async fn check_updates(client: &ApiClient, mode: OutputMode) -> CliResult<()> {
    let mut slugs: Vec<String> = linked::list()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.slug)
        .collect();
    slugs.sort();
    slugs.dedup();

    let mut notices: Vec<UpdateNotice> = Vec::new();
    for slug in &slugs {
        if let Ok(Some(skill)) = api_skills::find_by_slug(client, slug).await {
            if let Some(latest) = skill.current_version_semver.as_deref() {
                if let Some(n) = pending_update(
                    slug,
                    latest,
                    skill.owner_username.as_deref(),
                    skill.owner_team_id.is_some(),
                ) {
                    notices.push(n);
                }
            }
        }
    }

    if mode.json {
        emit_ok(
            mode,
            json!({
                "checked": slugs.len(),
                "updates": notices.iter().map(|n| json!({
                    "slug": n.slug, "have": n.have, "latest": n.latest, "author": n.author,
                })).collect::<Vec<_>>(),
            }),
            || {},
        );
        return Ok(());
    }
    if mode.quiet {
        return Ok(());
    }
    if slugs.is_empty() {
        println!("No skills linked.");
    } else if notices.is_empty() {
        println!("All {} linked skill(s) are up to date.", slugs.len());
    } else {
        for n in &notices {
            println!(
                "{} — have {}, latest {} (by {})",
                n.slug, n.have, n.latest, n.author
            );
        }
        println!("\nRun `knack link <slug>` to update one, or `knack link --all` for all.");
    }
    Ok(())
}

/// `knack link --all`: explicitly update every linked skill to its latest
/// published version (a user-initiated pull, not automatic). Re-downloads
/// and re-links only the copies whose version drifted.
async fn update_all(client: &ApiClient, mode: OutputMode) -> CliResult<()> {
    let entries = linked::list().unwrap_or_default();
    let mut slugs: Vec<String> = entries.iter().map(|s| s.slug.clone()).collect();
    slugs.sort();
    slugs.dedup();

    if slugs.is_empty() {
        if mode.json {
            emit_ok(mode, json!({ "checked": 0, "updated": [] }), || {});
        } else if !mode.quiet {
            println!("No skills linked; nothing to update.");
        }
        return Ok(());
    }

    let mut updated: Vec<String> = Vec::new();
    for slug in &slugs {
        // Latest published version for this slug.
        let latest = match api_skills::find_by_slug(client, slug).await {
            Ok(Some(s)) => s.current_version_semver,
            _ => None,
        };
        let Some(latest) = latest else { continue };
        // Skip ones already current.
        let already = linked::list()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.slug == *slug)
            .all(|s| s.version == latest);
        if already {
            continue;
        }
        if relink_to_latest(client, slug).await {
            updated.push(format!("{slug}@{latest}"));
        }
    }

    if mode.json {
        emit_ok(
            mode,
            json!({ "checked": slugs.len(), "updated": updated }),
            || {},
        );
        return Ok(());
    }
    if mode.quiet {
        return Ok(());
    }
    if updated.is_empty() {
        println!("All {} linked skill(s) already up to date.", slugs.len());
    } else {
        for r in &updated {
            println!("Updated {r}");
        }
    }
    Ok(())
}

/// Re-download `slug` and rewrite every linked copy (all recorded scopes)
/// to the latest version, updating the registry. Used by the explicit
/// `--all` path. Returns `true` if anything was rewritten.
async fn relink_to_latest(client: &ApiClient, slug: &str) -> bool {
    let entries: Vec<linked::LinkedSkill> = linked::list()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.slug == slug)
        .collect();
    if entries.is_empty() {
        return false;
    }
    let Ok(skill) = materialize(client, slug).await else {
        return false;
    };
    let fm = parse_skill_md_frontmatter(&skill.skill_md)
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut any = false;
    for entry in entries {
        let targets: Vec<&'static AgentTarget> =
            entry.agents.iter().filter_map(|a| targets::find(a)).collect();
        if targets.is_empty() {
            continue;
        }
        let write = write_skill_to_targets(&skill, &fm, entry.scope, &targets, false);
        if !write.written_agents.is_empty() {
            let _ = linked::add(&skill.slug, &skill.version, entry.scope, write.written_agents);
            any = true;
        }
    }
    any
}

// ─── unlink ────────────────────────────────────────────────────────────────

pub async fn run_unlink(args: UnlinkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let scope = resolve_scope(args.global, args.local, client.config.link_scope);

    // For removal we sweep every known target at this scope (not just the
    // installed set), so a skill linked before the install set changed is
    // still cleaned up. Sigil-protected removers never touch user files.
    let agent_targets: Vec<&'static AgentTarget> = match args.agent.as_deref() {
        Some(name) => match targets::find(name) {
            Some(t) => vec![t],
            None => {
                let err = unknown_agent_err(name);
                emit_err(mode, &err);
                return Err(err);
            }
        },
        None => targets::TARGETS.iter().collect(),
    };

    let mut removed: Vec<(String, String)> = Vec::new();
    for t in &agent_targets {
        let Some(root) = (t.shim_root)(scope) else {
            continue;
        };
        let did = match t.shim_style {
            ShimStyle::NativeSkill => shim::remove_linked_skill(&root, &args.slug).unwrap_or(false),
            ShimStyle::NativeRule => shim::remove_native_rule(&root, &args.slug).unwrap_or(false),
            ShimStyle::TextBlock => shim::remove_skill_block(&root, &args.slug).unwrap_or(false),
            ShimStyle::None => false,
        };
        if did {
            removed.push((t.name.to_string(), root.display().to_string()));
        }
    }

    let _ = linked::remove(&args.slug, scope);

    if mode.json {
        emit_ok(
            mode,
            json!({
                "slug": args.slug,
                "scope": scope.as_str(),
                "removed": removed
                    .iter()
                    .map(|(a, p)| json!({"agent": a, "path": p}))
                    .collect::<Vec<_>>(),
            }),
            || {},
        );
        return Ok(());
    }
    if mode.quiet {
        return Ok(());
    }
    if removed.is_empty() {
        println!(
            "Nothing to unlink: `{}` was not linked at {} scope in any known agent.",
            args.slug,
            scope.as_str()
        );
    } else {
        for (a, p) in &removed {
            println!("Unlinked {a} → {p}");
        }
    }
    Ok(())
}

// ─── shared helpers ──────────────────────────────────────────────────────────

fn resolve_scope(global: bool, local: bool, default: LinkScope) -> Scope {
    if global {
        Scope::Home
    } else if local {
        Scope::Project
    } else {
        match default {
            LinkScope::Home => Scope::Home,
            LinkScope::Project => Scope::Project,
        }
    }
}

fn unknown_agent_err(name: &str) -> CliError {
    CliError::User {
        code: "UNKNOWN_TARGET".into(),
        message: format!("unknown agent target: {name}"),
        hint: Some(format!("known: {}", targets::names().join(", "))),
    }
}

/// Agents to link into: one named target with `--agent`, else every agent
/// recorded as installed (deduped, registry-resolved). An empty installed
/// set is an error pointing at `knack install`.
fn resolve_targets(
    agent: Option<&str>,
    mode: OutputMode,
) -> CliResult<Vec<&'static AgentTarget>> {
    if let Some(name) = agent {
        return match targets::find(name) {
            Some(t) => Ok(vec![t]),
            None => {
                let err = unknown_agent_err(name);
                emit_err(mode, &err);
                Err(err)
            }
        };
    }
    let mut seen: HashSet<&'static str> = HashSet::new();
    let mut out: Vec<&'static AgentTarget> = Vec::new();
    for entry in installed::list().unwrap_or_default() {
        if let Some(t) = targets::find(&entry.slug) {
            if seen.insert(t.name) {
                out.push(t);
            }
        }
    }
    if out.is_empty() {
        let err = CliError::User {
            code: "NO_AGENTS_INSTALLED".into(),
            message: "no AI agents are registered with knack on this machine".into(),
            hint: Some(
                "run `knack install` first (or pass `--agent <name>` to target one explicitly)"
                    .into(),
            ),
        };
        emit_err(mode, &err);
        return Err(err);
    }
    Ok(out)
}

/// A skill version's files in memory plus its raw SKILL.md.
struct Materialized {
    slug: String,
    version: String,
    /// `(posix_relpath, bytes)` for every file in the bundle, including SKILL.md.
    files: Vec<(String, Vec<u8>)>,
    skill_md: String,
}

/// Download + assemble a skill version's files in memory. Returns errors
/// for the caller to surface (it does NOT emit) so the silent auto-refresh
/// path can ignore failures without polluting `knack run --json` output.
async fn materialize(client: &ApiClient, spec: &str) -> CliResult<Materialized> {
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return materialize_github(spec, local_path).await;
    }

    let (slug, version_filter) = crate::slug::parse_slug_at_version(spec);
    let skill = match api_skills::find_by_slug(client, slug).await {
        Ok(Some(s)) => s,
        Ok(None) => return Err(CliError::NotFound(format!("skill `{slug}` not found"))),
        Err(e) => return Err(e),
    };
    let semver = match version_filter {
        Some(v) => v.to_string(),
        None => skill
            .current_version_semver
            .clone()
            .ok_or_else(|| CliError::NotFound(format!("skill `{slug}` has no published version")))?,
    };
    let version = api_skills::get_version(client, &skill.id, &semver).await?;

    if version.packed_s3_key.is_some() {
        // V2a: download the tarball, unpack into a temp dir, read every file.
        let dl = api_skills::bundle_download(client, &skill.id, &version.version).await?;
        let resp = crate::http::client().get(&dl.url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(CliError::Server {
                status: status.as_u16(),
                code: "BUNDLE_DOWNLOAD_FAILED".into(),
                message: format!("R2 GET returned {status}"),
            });
        }
        let bytes = resp.bytes().await?;
        let tmp = tempfile::tempdir().map_err(CliError::from)?;
        unpack_skill(&bytes, tmp.path())?;
        let files = read_tree(tmp.path())?;
        let skill_md = files
            .iter()
            .find(|(rel, _)| rel == "SKILL.md")
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .ok_or_else(|| {
                CliError::Internal(format!("bundle for `{}` has no SKILL.md", skill.slug))
            })?;
        Ok(Materialized {
            slug: skill.slug,
            version: version.version,
            files,
            skill_md,
        })
    } else {
        // Legacy text-field version: assemble the file set by hand,
        // mirroring `pull`'s legacy path.
        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("SKILL.md".into(), version.skill_md.clone().into_bytes()),
            (
                "meta.knack.yaml".into(),
                version.meta_yaml.clone().into_bytes(),
            ),
        ];
        if !version.intuition_md.trim().is_empty() {
            files.push(("intuition.md".into(), version.intuition_md.clone().into_bytes()));
        }
        if !version.tests_yaml.trim().is_empty() {
            files.push((
                "tests/basic.yaml".into(),
                version.tests_yaml.clone().into_bytes(),
            ));
        }
        Ok(Materialized {
            slug: skill.slug,
            version: version.version,
            files,
            skill_md: version.skill_md,
        })
    }
}

async fn materialize_github(spec: &str, local_path: &std::path::Path) -> CliResult<Materialized> {
    use knack_backend_github::GithubBackend;
    use knack_types::Backend;

    let (slug, version_filter) = crate::slug::parse_slug_at_version(spec);
    let backend = GithubBackend::new("".to_string(), "".to_string(), local_path.to_path_buf());
    let pkg = match backend.pull(slug, version_filter).await {
        Ok(p) => p,
        Err(e) => return Err(CliError::NotFound(format!("github pull: {e}"))),
    };
    let files: Vec<(String, Vec<u8>)> = pkg
        .files
        .iter()
        .map(|f| (posix(&f.path), f.bytes.clone()))
        .collect();
    let skill_md = files
        .iter()
        .find(|(rel, _)| rel == "SKILL.md")
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        .ok_or_else(|| CliError::Internal(format!("skill `{}` has no SKILL.md", pkg.slug)))?;
    Ok(Materialized {
        slug: pkg.slug,
        version: pkg.version,
        files,
        skill_md,
    })
}

/// Walk `root` and return `(posix_relpath, bytes)` for every file.
fn read_tree(root: &std::path::Path) -> CliResult<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.map_err(|e| CliError::Internal(format!("walk bundle: {e}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| CliError::Internal(format!("strip prefix: {e}")))?;
        let bytes = std::fs::read(entry.path()).map_err(CliError::from)?;
        out.push((posix(rel), bytes));
    }
    Ok(out)
}

fn posix(p: &std::path::Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// One-line note when a same-name link in the other scope means Claude
/// Code's "personal overrides project" precedence will shadow one of them.
fn precedence_warning(slug: &str, scope: Scope) -> Option<String> {
    let other = match scope {
        Scope::Home => Scope::Project,
        Scope::Project => Scope::Home,
    };
    let exists_other = linked::list()
        .unwrap_or_default()
        .iter()
        .any(|s| s.slug == slug && s.scope == other);
    if !exists_other {
        return None;
    }
    Some(match scope {
        Scope::Home => format!(
            "note: `{slug}` is also linked at project scope; the global copy you just \
             linked takes precedence (personal overrides project)."
        ),
        Scope::Project => format!(
            "note: `{slug}` is also linked globally; that global copy takes precedence \
             over this project one (personal overrides project)."
        ),
    })
}

// ─── reporting ───────────────────────────────────────────────────────────────

struct AgentOutcome {
    agent: &'static str,
    display: &'static str,
    path: Option<PathBuf>,
    status: &'static str,
    reason: Option<String>,
}

impl AgentOutcome {
    fn wrote(t: &AgentTarget, path: PathBuf) -> Self {
        Self {
            agent: t.name,
            display: t.display,
            path: Some(path),
            status: "linked",
            reason: None,
        }
    }
    fn dry_run(t: &AgentTarget, path: PathBuf) -> Self {
        Self {
            agent: t.name,
            display: t.display,
            path: Some(path),
            status: "dry_run",
            reason: None,
        }
    }
    fn skipped(t: &AgentTarget, path: Option<PathBuf>, reason: &str) -> Self {
        Self {
            agent: t.name,
            display: t.display,
            path,
            status: "skipped",
            reason: Some(reason.to_string()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_link_report(
    mode: OutputMode,
    slug: &str,
    version: &str,
    scope: Scope,
    outcomes: &[AgentOutcome],
    dry_run: bool,
    created_top_level_dir: bool,
    precedence_note: Option<&str>,
) {
    if mode.json {
        emit_ok(
            mode,
            json!({
                "slug": slug,
                "version": version,
                "scope": scope.as_str(),
                "dry_run": dry_run,
                "restart_required": created_top_level_dir,
                "precedence_note": precedence_note,
                "agents": outcomes
                    .iter()
                    .map(|o| json!({
                        "agent": o.agent,
                        "path": o.path.as_ref().map(|p| p.display().to_string()),
                        "status": o.status,
                        "reason": o.reason,
                    }))
                    .collect::<Vec<_>>(),
            }),
            || {},
        );
        return;
    }
    if mode.quiet {
        return;
    }
    let verb = if dry_run { "Would link" } else { "Linked" };
    for o in outcomes {
        match o.status {
            "linked" | "dry_run" => println!(
                "{verb} {} ({}) → {}",
                slug,
                o.display,
                o.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            _ => println!(
                "Skipped {} ({}){}",
                o.display,
                o.reason.as_deref().unwrap_or("?"),
                o.path
                    .as_ref()
                    .map(|p| format!(" — {}", p.display()))
                    .unwrap_or_default()
            ),
        }
    }
    if let Some(note) = precedence_note {
        println!("{note}");
    }
    if created_top_level_dir && !dry_run {
        chatter(
            mode,
            "a new agent skills directory was created; restart the agent (e.g. Claude Code) \
             so it watches the new directory and the slash command appears.",
        );
    }
}

fn list_linked(mode: OutputMode) -> CliResult<()> {
    let skills = linked::list().unwrap_or_default();
    if mode.json {
        emit_ok(
            mode,
            json!({
                "linked": skills
                    .iter()
                    .map(|s| json!({
                        "slug": s.slug,
                        "version": s.version,
                        "scope": s.scope.as_str(),
                        "agents": s.agents,
                    }))
                    .collect::<Vec<_>>(),
            }),
            || {},
        );
        return Ok(());
    }
    if mode.quiet {
        return Ok(());
    }
    if skills.is_empty() {
        println!("No skills linked. Use `knack link <slug>` to install one as a slash command.");
        return Ok(());
    }
    for s in &skills {
        println!(
            "{}@{} [{}] → {}",
            s.slug,
            s.version,
            s.scope.as_str(),
            s.agents.join(", ")
        );
    }
    Ok(())
}
