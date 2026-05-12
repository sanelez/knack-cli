//! Agent integration targets.
//!
//! Each entry maps a coding-agent runtime to the file `knack install` should
//! write to. Adding a new agent is a single struct literal: declare the env
//! marker that proves it's running (or `&[]` if there isn't one), the binary
//! name to look for on PATH, the config-path resolver, and the write style.
//!
//! Ordering matters: `autodetect()` walks this list and returns the first
//! match, so more-specific agents (Claude Code, Codex) come before
//! "everything that writes AGENTS.md" generics.

use std::path::PathBuf;

use super::installed::Scope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigStyle {
    /// Append a delimited markdown block to the file. Idempotent: re-runs
    /// splice in place; original surrounding content is preserved.
    AppendBlock,
    /// Write the whole file (used for Cursor `.mdc` rules which need
    /// frontmatter and are owned end-to-end by knack).
    WriteFile,
}

/// How per-skill shims for this target are written.
///
/// Distinct from [`ConfigStyle`] — that one governs the one-time install
/// block (`knack exists, run knack info`). Shims are the per-pulled-skill
/// registrations that make the runtime's native discovery surface the
/// skill on session start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimStyle {
    /// Runtime natively reads Anthropic Skills (Claude Code, Cowork).
    /// One folder per skill at `<root>/<slug>/SKILL.md` with frontmatter.
    NativeSkill,
    /// Runtime natively reads rule files keyed by description (Cursor).
    /// One file per skill at `<root>/knack-<slug>.mdc`.
    NativeRule,
    /// Runtime reads a free-form context file (AGENTS.md, CONVENTIONS.md).
    /// We splice a small per-skill block into that file.
    TextBlock,
    /// Runtime has no useful discovery mechanism for individual skills.
    /// `generic` is the only target that uses this — the install block
    /// telling the agent "knack exists" is the best we can do.
    None,
}

pub struct AgentTarget {
    /// Slug accepted by `knack install <name>` and reported in output.
    pub name: &'static str,
    /// Human-readable label.
    pub display: &'static str,
    /// Environment variables that prove this agent is currently running.
    /// Any of them being set is sufficient.
    pub env_markers: &'static [&'static str],
    /// Binaries that, if present on PATH, indicate this agent is installed.
    pub binary_markers: &'static [&'static str],
    /// Where to write the install block. Returns `None` if the OS doesn't
    /// have a reasonable destination (e.g. no home directory).
    pub config_path: fn() -> Option<PathBuf>,
    /// How to write the install block.
    pub style: ConfigStyle,
    /// How to write per-skill shims.
    pub shim_style: ShimStyle,
    /// Where per-skill shims live, scoped to home (HOME-shared pool) vs
    /// project (workspace-local). Returns `None` when this combination
    /// doesn't make sense (e.g. `generic` has no native shim story; some
    /// project-only runtimes return `None` for `Home`).
    pub shim_root: fn(Scope) -> Option<PathBuf>,
}

/// Ordered registry. Autodetect walks this list top-down; first env hit wins,
/// then first binary hit wins. `generic` is always last and is added as a
/// safety net by the caller (excluded from autodetect itself).
pub static TARGETS: &[AgentTarget] = &[
    AgentTarget {
        name: "claude",
        display: "Claude Code / Claude Cowork",
        // CLAUDECODE=1 is set by Claude Code on every shell it spawns;
        // CLAUDE_CODE_IS_COWORK is the internal flag Cowork sets to enable
        // eager-flush behavior. Cowork shares the same ~/.claude/CLAUDE.md
        // context system, so one target covers both.
        env_markers: &["CLAUDECODE", "CLAUDE_CODE_IS_COWORK"],
        binary_markers: &["claude"],
        config_path: claude_path,
        style: ConfigStyle::AppendBlock,
        // Claude Code is the only May-2026 runtime with native Anthropic
        // Skills discovery. Skills land at <root>/.claude/skills/<slug>/
        // SKILL.md and are picked up via progressive disclosure on
        // session start.
        shim_style: ShimStyle::NativeSkill,
        shim_root: claude_shim_root,
    },
    AgentTarget {
        name: "codex",
        display: "OpenAI Codex CLI",
        env_markers: &["CODEX_HOME"],
        binary_markers: &["codex"],
        config_path: codex_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: codex_shim_root,
    },
    AgentTarget {
        name: "cursor",
        display: "Cursor",
        env_markers: &["CURSOR_TRACE_ID"],
        binary_markers: &["cursor"],
        config_path: cursor_path,
        style: ConfigStyle::WriteFile,
        // Cursor has native rule discovery (`.cursor/rules/*.mdc`); per
        // confirmed choice we ship one .mdc per skill.
        shim_style: ShimStyle::NativeRule,
        shim_root: cursor_shim_root,
    },
    AgentTarget {
        name: "windsurf",
        display: "Windsurf (Cascade)",
        env_markers: &[],
        binary_markers: &["windsurf"],
        config_path: windsurf_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: windsurf_shim_root,
    },
    AgentTarget {
        name: "cline",
        // Cline is a VS Code extension; no env or binary marker on the
        // host shell. Users invoke `knack install cline` manually. We write
        // a dedicated file under .clinerules/ which Cline auto-loads.
        display: "Cline",
        env_markers: &[],
        binary_markers: &[],
        config_path: cline_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: cline_shim_root,
    },
    AgentTarget {
        name: "continue",
        // Continue.dev: same story as Cline. Write a discrete file under
        // .continue/rules/ which the extension auto-loads at activation.
        display: "Continue.dev",
        env_markers: &[],
        binary_markers: &[],
        config_path: continue_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: continue_shim_root,
    },
    AgentTarget {
        name: "kiro",
        display: "Kiro (AWS)",
        env_markers: &[],
        binary_markers: &["kiro"],
        config_path: kiro_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: kiro_shim_root,
    },
    AgentTarget {
        name: "trae",
        display: "Trae (ByteDance)",
        env_markers: &[],
        binary_markers: &["trae"],
        config_path: trae_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: trae_shim_root,
    },
    AgentTarget {
        name: "aider",
        display: "Aider",
        env_markers: &[],
        binary_markers: &["aider"],
        config_path: aider_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: aider_shim_root,
    },
    AgentTarget {
        name: "gemini",
        display: "Gemini CLI",
        env_markers: &["GEMINI_CLI"],
        binary_markers: &["gemini"],
        config_path: gemini_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: gemini_shim_root,
    },
    AgentTarget {
        name: "opencode",
        display: "OpenCode",
        env_markers: &[],
        binary_markers: &["opencode"],
        config_path: opencode_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: opencode_shim_root,
    },
    AgentTarget {
        name: "factory",
        display: "Factory droid",
        env_markers: &["FACTORY_DROID"],
        binary_markers: &["droid"],
        config_path: factory_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: factory_shim_root,
    },
    AgentTarget {
        name: "amp",
        display: "Amp",
        env_markers: &[],
        binary_markers: &["amp"],
        config_path: amp_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: amp_shim_root,
    },
    AgentTarget {
        name: "generic",
        display: "Generic AGENTS.md",
        env_markers: &[],
        binary_markers: &[],
        config_path: generic_path,
        style: ConfigStyle::AppendBlock,
        // `generic` is the AGENTS.md fallback — there's no specific
        // runtime to register with, so the install block is the entire
        // surface. Per-skill shims would just duplicate it.
        shim_style: ShimStyle::None,
        shim_root: |_| None,
    },
];

pub fn find(name: &str) -> Option<&'static AgentTarget> {
    TARGETS.iter().find(|t| t.name == name)
}

pub fn names() -> Vec<&'static str> {
    TARGETS.iter().map(|t| t.name).collect()
}

// ─── Config-path resolvers ─────────────────────────────────────────────────

fn claude_path() -> Option<PathBuf> {
    // Claude Code honors $CLAUDE_CONFIG_DIR; default is ~/.claude.
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("CLAUDE.md"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("CLAUDE.md"))
}

fn codex_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        return Some(PathBuf::from(dir).join("AGENTS.md"));
    }
    dirs::home_dir().map(|h| h.join(".codex").join("AGENTS.md"))
}

fn cursor_path() -> Option<PathBuf> {
    // Cursor's 2026 rules format is project-scoped at .cursor/rules/*.mdc.
    // If the user runs `knack install` inside a git repo, put it there. If
    // not, fall back to a user-scope location they can copy from later.
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".git").exists() {
            return Some(cwd.join(".cursor").join("rules").join("knack.mdc"));
        }
    }
    dirs::home_dir().map(|h| h.join(".cursor").join("rules").join("knack.mdc"))
}

fn aider_path() -> Option<PathBuf> {
    // Aider reads CONVENTIONS.md at the repo root when invoked with --read.
    std::env::current_dir().ok().map(|c| c.join("CONVENTIONS.md"))
}

fn windsurf_path() -> Option<PathBuf> {
    // Windsurf (Cascade) auto-loads AGENTS.md at the project root as an
    // always-on rule. Falls back to CWD if not in a git repo.
    std::env::current_dir().ok().map(|c| c.join("AGENTS.md"))
}

fn cline_path() -> Option<PathBuf> {
    // Cline auto-loads every file in .clinerules/. Writing a dedicated
    // knack.md keeps our block scoped and out of the way of the user's own
    // .clinerules content.
    std::env::current_dir()
        .ok()
        .map(|c| c.join(".clinerules").join("knack.md"))
}

fn continue_path() -> Option<PathBuf> {
    // Continue.dev auto-loads every file in .continue/rules/.
    std::env::current_dir()
        .ok()
        .map(|c| c.join(".continue").join("rules").join("knack.md"))
}

fn kiro_path() -> Option<PathBuf> {
    // Kiro (AWS) reads AGENTS.md from ~/.kiro/steering/ as global guidance
    // applied to every workspace.
    dirs::home_dir().map(|h| h.join(".kiro").join("steering").join("AGENTS.md"))
}

fn trae_path() -> Option<PathBuf> {
    // Trae (ByteDance) uses .trae/rules/project_rules.md as the project
    // rules file. AppendBlock preserves any existing user content.
    std::env::current_dir()
        .ok()
        .map(|c| c.join(".trae").join("rules").join("project_rules.md"))
}

fn gemini_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini").join("GEMINI.md"))
}

fn opencode_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".opencode").join("AGENTS.md"))
}

fn factory_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".factory").join("AGENTS.md"))
}

fn amp_path() -> Option<PathBuf> {
    // Amp uses $XDG_CONFIG_HOME/AGENTS.md (Linux/macOS) / %APPDATA%\AGENTS.md
    // (Windows). `dirs::config_dir()` returns the right thing on every OS.
    dirs::config_dir().map(|c| c.join("AGENTS.md"))
}

fn generic_path() -> Option<PathBuf> {
    // The agents.md proposed standard: $XDG_CONFIG_HOME/agents/AGENTS.md or
    // %APPDATA%\agents\AGENTS.md.
    dirs::config_dir().map(|c| c.join("agents").join("AGENTS.md"))
}

// ─── Shim-root resolvers ───────────────────────────────────────────────────
//
// For NativeSkill / NativeRule styles, the shim root is a *directory* under
// which we write one folder (or .mdc file) per pulled skill. For TextBlock
// targets the "root" is the same file we already wrote the install block
// into — shims are sentinel-bracketed blocks appended after the install
// block. Both home and project scopes are supported where meaningful.

fn claude_shim_root(scope: Scope) -> Option<PathBuf> {
    let dir = match scope {
        Scope::Home => {
            if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
                PathBuf::from(d).join("skills")
            } else {
                dirs::home_dir()?.join(".claude").join("skills")
            }
        }
        // Workspace: Claude Code also scans <cwd>/.claude/skills/ for
        // project-scoped tools. CWD is the right anchor — the shim
        // writer is invoked from the same process as `knack pull`, which
        // already resolved its workspace via discovery.
        Scope::Project => std::env::current_dir().ok()?.join(".claude").join("skills"),
    };
    Some(dir)
}

fn cursor_shim_root(scope: Scope) -> Option<PathBuf> {
    let dir = match scope {
        // Cursor is fundamentally per-project — rules live in
        // <workspace>/.cursor/rules. HOME scope is supported only as a
        // fallback for users who have Cursor installed but no current
        // project; rare, but consistent with how cursor_path picks a
        // location.
        Scope::Home => dirs::home_dir()?.join(".cursor").join("rules"),
        Scope::Project => std::env::current_dir().ok()?.join(".cursor").join("rules"),
    };
    Some(dir)
}

// TextBlock targets: shim root == the install-block file itself. The
// shim writer for TextBlock splices `<!-- knack:skill:<slug>:start -->`
// pairs into that file. We surface the file path here so shim sync
// has a single uniform interface.

fn codex_shim_root(_: Scope) -> Option<PathBuf> { codex_path() }
fn windsurf_shim_root(_: Scope) -> Option<PathBuf> { windsurf_path() }
fn cline_shim_root(_: Scope) -> Option<PathBuf> { cline_path() }
fn continue_shim_root(_: Scope) -> Option<PathBuf> { continue_path() }
fn kiro_shim_root(_: Scope) -> Option<PathBuf> { kiro_path() }
fn trae_shim_root(_: Scope) -> Option<PathBuf> { trae_path() }
fn aider_shim_root(_: Scope) -> Option<PathBuf> { aider_path() }
fn gemini_shim_root(_: Scope) -> Option<PathBuf> { gemini_path() }
fn opencode_shim_root(_: Scope) -> Option<PathBuf> { opencode_path() }
fn factory_shim_root(_: Scope) -> Option<PathBuf> { factory_path() }
fn amp_shim_root(_: Scope) -> Option<PathBuf> { amp_path() }
