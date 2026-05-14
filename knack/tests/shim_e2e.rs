//! End-to-end shim sync.
//!
//! We don't fork a `knack pull` here — the API mock + auth dance would
//! triple the test surface. Instead, the canonical `.knack/skills/<slug>/`
//! is written directly to a temp workspace and we drive `sync_one_skill`
//! the same way `commands/pull.rs` does. That covers everything from
//! "frontmatter parse → render → write to runtime shim dir → R2/cache
//! agnostic" without the network.

use std::fs;
use std::path::PathBuf;

use knack_cli::commands::install::installed::{self, Scope};
use knack_cli::commands::sync::sync_one_skill;
use knack_cli::config::Config;

fn iso() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // Isolate every env-derived path away from the developer's real
    // ~/.knack so a test run never clobbers their state. Each variable
    // covers one HOME-scope-resolving function in targets.rs — extend
    // as new targets need overrides.
    //
    // KNACK_TEST_HOME and KNACK_TEST_CONFIG redirect `dirs::home_dir()`
    // / `dirs::config_dir()` for every NativeSkill target that reads
    // them (codex's ~/.agents/, gemini's ~/.gemini/, cline's ~/.cline/,
    // kiro's ~/.kiro/, factory's ~/.factory/, opencode's ~/.config/
    // opencode/, amp's ~/.config/agents/, etc.). Keeping a single pair
    // of env vars is cleaner than a custom override per target.
    unsafe {
        std::env::set_var("KNACK_INSTALLED_FILE", dir.path().join("installed.json"));
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().join("home_claude"));
        std::env::set_var("CODEX_HOME", dir.path().join("home_codex"));
        std::env::set_var("KNACK_SKILLS_DIR", dir.path().join("home_skills"));
        std::env::set_var("KNACK_TEST_HOME", dir.path().join("home"));
        std::env::set_var("KNACK_TEST_CONFIG", dir.path().join("home").join(".config"));
    }
    dir
}

fn write_canonical_skill(root: &std::path::Path, slug: &str, description: &str) -> PathBuf {
    let dir = root.join(".knack").join("skills").join(slug);
    fs::create_dir_all(&dir).unwrap();
    let skill_md = format!(
        "---\nname: {slug}\ndescription: {description}\n---\n\n# {slug}\n\nBody.\n"
    );
    fs::write(dir.join("SKILL.md"), &skill_md).unwrap();
    dir
}

#[test]
fn sync_one_skill_writes_claude_shim_at_project_scope() {
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();

    let canonical = write_canonical_skill(tmp.path(), "monthly-close", "Reconciles Mondays.");
    installed::add(
        "claude",
        Scope::Project,
        tmp.path().join("CLAUDE.md"),
    )
    .unwrap();

    let report = sync_one_skill("monthly-close", Scope::Project, &Config::load());

    assert!(
        report.skipped.is_empty(),
        "no per-target skips expected: {:?}",
        report.skipped
    );
    assert_eq!(report.written.len() + report.up_to_date.len(), 1);

    let shim = tmp
        .path()
        .join(".claude")
        .join("skills")
        .join("monthly-close")
        .join("SKILL.md");
    assert!(shim.is_file(), "shim should exist at {}", shim.display());

    let body = fs::read_to_string(&shim).unwrap();
    assert!(body.starts_with("<!-- knack:shim"));
    assert!(body.contains("description: Reconciles Mondays."));
    assert!(body.contains("knack run monthly-close"));
    // Pointer to canonical body present.
    assert!(body.contains(&canonical.join("SKILL.md").display().to_string()));
}

#[test]
fn sync_one_skill_skips_when_canonical_missing() {
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();
    installed::add(
        "claude",
        Scope::Project,
        tmp.path().join("CLAUDE.md"),
    )
    .unwrap();

    // No canonical write — sync should skip gracefully.
    let report = sync_one_skill("nope", Scope::Project, &Config::load());
    assert_eq!(report.skipped.len(), 1);
    assert!(report
        .skipped
        .first()
        .unwrap()
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("skill folder not found"));
}

#[test]
fn sync_with_no_recorded_agents_is_a_clean_noop() {
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();
    write_canonical_skill(tmp.path(), "weekly-digest", "Top tickets.");
    // installed.json empty.
    let report = sync_one_skill("weekly-digest", Scope::Project, &Config::load());
    assert!(report.written.is_empty());
    assert!(report.up_to_date.is_empty());
    assert!(report.skipped.is_empty());
}

#[test]
fn home_installed_textblock_agent_catches_project_pull() {
    // Regression guard for the Aider/Continue scope mismatch: a
    // HOME-installed text-context agent must receive shims when the
    // user pulls a skill into a workspace, because that agent reads
    // the HOME file globally on every session.
    //
    // Why aider and not codex: codex moved to NativeSkill in May 2026
    // (`.agents/skills/` is its real discovery path, not AGENTS.md).
    // Aider stays TextBlock — no native SKILL.md support in mainline.
    // Aider's path is `<cwd>/CONVENTIONS.md`; we install it at HOME
    // scope but `aider_shim_root` returns the same workspace path
    // regardless of scope, so the test verifies the matches_scope
    // permissiveness specifically.
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();

    write_canonical_skill(tmp.path(), "monthly-close", "Reconciles Mondays.");

    // Aider is now HOME-anchored (`~/CONVENTIONS.md`). KNACK_TEST_HOME
    // (set in iso()) redirects ~/ to tmp/home/, so the actual file path
    // is tmp/home/CONVENTIONS.md. The shim writer must hit that exact
    // location regardless of the pull scope, which is the property
    // under test.
    let aider_conventions = tmp.path().join("home").join("CONVENTIONS.md");
    fs::create_dir_all(aider_conventions.parent().unwrap()).unwrap();
    fs::write(
        &aider_conventions,
        "<!-- knack:start (managed by `knack install` — do not edit between markers) -->\n\
         The knack CLI is on this machine.\n\
         <!-- knack:end -->\n",
    )
    .unwrap();
    installed::add("aider", Scope::Home, aider_conventions.clone()).unwrap();

    // Pull happens at PROJECT scope.
    let report = sync_one_skill("monthly-close", Scope::Project, &Config::load());

    // Even though scopes don't match, the TextBlock-style aider entry
    // should catch the pull and splice a per-skill block into the
    // workspace CONVENTIONS.md.
    assert!(
        report.written.iter().any(|r| r.agent == "aider"),
        "expected aider shim, got report: {report:?}"
    );

    let body = fs::read_to_string(&aider_conventions).unwrap();
    assert!(body.contains("<!-- knack:skill:monthly-close:start -->"));
    assert!(body.contains("Reconciles Mondays."));
    assert!(body.contains("knack run monthly-close"));
}

#[test]
fn home_installed_nativeskill_agent_receives_project_pull_into_workspace_root() {
    // Global-by-default install: the user installs Knack once at HOME
    // (`knack install` writes ~/.claude/CLAUDE.md). They later pull a
    // skill in a workspace. The per-skill shim should land in
    // <workspace>/.claude/skills/<slug>/, NOT ~/.claude/skills/ — so
    // the skill doesn't bleed into other projects, but the global
    // install still services every workspace.
    //
    // This is the install-scope/pull-scope decoupling: the entry's
    // scope says "where the install block lives," and the pull scope
    // says "where the shim goes." They are independent.
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();

    write_canonical_skill(tmp.path(), "monthly-close", "Reconciles Mondays.");
    installed::add(
        "claude",
        Scope::Home,
        tmp.path().join("home_claude").join("CLAUDE.md"),
    )
    .unwrap();

    let report = sync_one_skill("monthly-close", Scope::Project, &Config::load());

    assert!(
        report.written.iter().any(|r| r.agent == "claude"),
        "claude shim should be written at workspace scope; report: {report:?}"
    );
    let workspace_shim = tmp
        .path()
        .join(".claude")
        .join("skills")
        .join("monthly-close")
        .join("SKILL.md");
    assert!(
        workspace_shim.is_file(),
        "expected workspace shim at {}",
        workspace_shim.display()
    );
    // HOME skill folder should NOT have been written — the project
    // pull stays scoped to the workspace.
    let home_shim = tmp
        .path()
        .join("home_claude")
        .join("skills")
        .join("monthly-close")
        .join("SKILL.md");
    assert!(
        !home_shim.exists(),
        "project pull leaked into HOME at {}",
        home_shim.display()
    );
}

#[test]
fn sync_writes_cursor_mdc_when_cursor_installed() {
    let tmp = iso();
    std::env::set_current_dir(tmp.path()).unwrap();
    write_canonical_skill(tmp.path(), "tool-use", "Adapted recipes.");
    installed::add(
        "cursor",
        Scope::Project,
        tmp.path().join(".cursor").join("rules").join("knack.mdc"),
    )
    .unwrap();

    let report = sync_one_skill("tool-use", Scope::Project, &Config::load());
    assert!(
        report.skipped.is_empty(),
        "no per-target skips expected: {:?}",
        report.skipped
    );

    let mdc = tmp
        .path()
        .join(".cursor")
        .join("rules")
        .join("knack-tool-use.mdc");
    assert!(mdc.is_file(), "expected {} to exist", mdc.display());

    let body = fs::read_to_string(&mdc).unwrap();
    assert!(body.starts_with("<!-- knack:shim"));
    assert!(body.contains("alwaysApply: false"));
    assert!(body.contains("description: Adapted recipes."));
}
