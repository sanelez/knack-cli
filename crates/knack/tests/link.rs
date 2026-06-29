//! End-to-end test for `knack link` / `knack unlink` against a wiremock
//! server. Drives the cloud legacy path (no R2 bundle) so the test is
//! self-contained, and redirects every on-disk location at a tempdir via
//! the test env overrides (`CLAUDE_CONFIG_DIR`, `KNACK_INSTALLED_FILE`,
//! `KNACK_LINKED_FILE`) so it never touches the developer's real config.

use std::sync::Mutex;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::commands::link::{run_link, run_unlink, LinkArgs, UnlinkArgs};
use knack_cli::output::OutputMode;

mod common;

/// Env overrides are process-global; serialize the (single) test that
/// mutates them so a future second test here can't race.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn quiet() -> OutputMode {
    OutputMode {
        json: false,
        quiet: true,
        no_color: true,
    }
}

// The guard is held across awaits on purpose: it serializes mutation of the
// process-global test env vars for the whole test body. Harmless with a
// single test in this binary.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn link_writes_wrapped_skill_then_unlink_removes_it() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join("claude");
    let installed = tmp.path().join("installed.json");
    let linked = tmp.path().join("linked.json");

    // SAFETY: ENV_LOCK serializes access to these process-global vars for
    // the duration of this test.
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_dir);
        std::env::set_var("KNACK_INSTALLED_FILE", &installed);
        std::env::set_var("KNACK_LINKED_FILE", &linked);
    }

    // Seed installed.json so `link` resolves Claude as a target.
    std::fs::write(
        &installed,
        json!({
            "version": 1,
            "agents": [{"slug": "claude", "scope": "home", "path": "x"}],
        })
        .to_string(),
    )
    .unwrap();

    let (server, client, _store) = common::fixture().await;

    // find_by_slug → one matching skill with a current version.
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "sk1",
                "slug": "demo",
                "name": "Demo",
                "scope": "personal",
                "owner_user_id": "u1",
                "owner_team_id": null,
                "current_version_id": "v1",
                "current_version_semver": "1.0.0",
                "created_at": "2026-06-01T00:00:00Z",
            }],
            "next_cursor": null,
        })))
        .mount(&server)
        .await;

    // get_version → a legacy text-field version (no packed_s3_key) so we
    // skip the R2 bundle download entirely.
    Mock::given(method("GET"))
        .and(path("/skills/sk1/versions/1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "v1",
            "skill_id": "sk1",
            "version": "1.0.0",
            "skill_md": "---\nname: demo\ndescription: A demo skill.\n---\n\n# Demo\n\nDo the work.\n",
            "intuition_md": "",
            "meta_yaml": "slug: demo\nname: Demo\n",
            "parent_version_id": null,
            "created_by": "u1",
            "created_at": "2026-06-01T00:00:00Z",
        })))
        .mount(&server)
        .await;

    // Link globally (default scope) into the installed Claude target.
    run_link(
        LinkArgs {
            slug_at_version: Some("demo".into()),
            global: true,
            local: false,
            agent: None,
            print: false,
            force: false,
            list: false,
            check: false,
            all: false,
        },
        client.clone(),
        quiet(),
    )
    .await
    .expect("link succeeds");

    let skill_md = claude_dir.join("skills").join("demo").join("SKILL.md");
    let meta = claude_dir.join("skills").join("demo").join("meta.knack.yaml");
    assert!(skill_md.is_file(), "SKILL.md should be written at {skill_md:?}");
    assert!(meta.is_file(), "support file meta.knack.yaml should be copied");

    let body = std::fs::read_to_string(&skill_md).unwrap();
    // Frontmatter preserved and first.
    assert!(body.starts_with("---\nname: demo"));
    assert!(body.contains("description: A demo skill."));
    // Telemetry wrapper baked in with slug + runtime.
    assert!(body.contains("knack run demo --runtime claude --json"));
    assert!(body.contains("knack mark <run_id> succeeded"));
    // Original body survived.
    assert!(body.contains("# Demo"));

    // linked.json recorded the entry.
    let reg = std::fs::read_to_string(&linked).unwrap();
    assert!(reg.contains("\"demo\""));
    assert!(reg.contains("claude"));

    // Notify-only update check (no pulling). When the linked version matches
    // the latest, there's no notice.
    assert!(
        knack_cli::commands::link::pending_update("demo", "1.0.0", Some("jordan"), false).is_none(),
        "no update notice expected when linked version == latest"
    );
    // When a newer version is published upstream, we get a flag naming the
    // version and author — but nothing on disk changes (pull stays manual).
    let notice = knack_cli::commands::link::pending_update("demo", "1.1.0", Some("jordan"), false)
        .expect("a newer upstream version should produce a notice");
    assert_eq!(notice.have, "1.0.0");
    assert_eq!(notice.latest, "1.1.0");
    assert_eq!(notice.author, "jordan");
    // Disk untouched by the check: still the originally linked body.
    assert!(std::fs::read_to_string(&skill_md).unwrap().starts_with("---\nname: demo"));

    // Unlink removes the whole folder.
    run_unlink(
        UnlinkArgs {
            slug: "demo".into(),
            global: true,
            local: false,
            agent: None,
        },
        client.clone(),
        quiet(),
    )
    .await
    .expect("unlink succeeds");

    assert!(
        !claude_dir.join("skills").join("demo").exists(),
        "linked skill folder should be gone after unlink"
    );

    unsafe {
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::remove_var("KNACK_INSTALLED_FILE");
        std::env::remove_var("KNACK_LINKED_FILE");
    }
}
