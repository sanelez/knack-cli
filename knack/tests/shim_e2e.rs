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
    // ~/.knack so a test run never clobbers their state.
    unsafe {
        std::env::set_var("KNACK_INSTALLED_FILE", dir.path().join("installed.json"));
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().join("home_claude"));
        std::env::set_var("KNACK_SKILLS_DIR", dir.path().join("home_skills"));
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
