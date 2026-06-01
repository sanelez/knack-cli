//! Smoke tests — pin the cross-cutting CLI contracts that agents and
//! CI scripts depend on.
//!
//! Unlike `runs.rs` / `skills.rs` (which cover deep behavior of specific
//! API surfaces), this file covers the wiring that everything else
//! depends on: exit codes, error code strings, JSON envelope shape,
//! and the v0.7.x additions (`Partial` variant + exit 6, bulk-mark
//! partial response parsing, overview regression block, PAT 401-no-
//! refresh).
//!
//! If a smoke test here fails, it usually means a public contract
//! broke — bump the version, write a CHANGELOG entry, then re-roll.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::errors::{CliError, ExitCode};
use knack_cli::output::{err_envelope, ok_envelope, SCHEMA};

mod common;

// ── Exit code stability ────────────────────────────────────────────────
//
// These numbers are documented in `knack docs exit-codes` and consumed by
// CI scripts users have written against them. Re-numbering breaks every
// downstream consumer silently — at minimum, exit 6 (partial) was added
// in v0.7.10 and is now load-bearing for bulk-mark + future bulk-export.

#[test]
fn exit_codes_pin_v0_7_10_table() {
    assert_eq!(ExitCode::SUCCESS.0, 0);
    assert_eq!(ExitCode::USER.0, 1);
    assert_eq!(ExitCode::AUTH.0, 2);
    assert_eq!(ExitCode::NETWORK.0, 3);
    assert_eq!(ExitCode::CONFLICT.0, 4);
    assert_eq!(ExitCode::PLAN.0, 5);
    assert_eq!(ExitCode::PARTIAL.0, 6, "exit 6 is the bulk-mark partial-failure signal added in v0.7.10");
    assert_eq!(ExitCode::USAGE.0, 64);
    assert_eq!(ExitCode::INTERNAL.0, 70);
}

#[test]
fn every_error_variant_maps_to_documented_exit_code() {
    let cases: Vec<(CliError, ExitCode, &str)> = vec![
        (CliError::AuthRequired, ExitCode::AUTH, "AUTH_REQUIRED"),
        (
            CliError::AuthFailed("revoked".into()),
            ExitCode::AUTH,
            "AUTH_FAILED",
        ),
        (
            CliError::Network("conn refused".into()),
            ExitCode::NETWORK,
            "NETWORK",
        ),
        (
            CliError::Conflict {
                message: "version exists".into(),
                hint: None,
            },
            ExitCode::CONFLICT,
            "CONFLICT",
        ),
        (
            CliError::PlanLimit {
                message: "cap hit".into(),
                hint: None,
            },
            ExitCode::PLAN,
            "PLAN_LIMIT_EXCEEDED",
        ),
        (
            CliError::NotFound("run nope".into()),
            ExitCode::USER,
            "NOT_FOUND",
        ),
        (
            CliError::Server {
                status: 500,
                code: "INTERNAL".into(),
                message: "boom".into(),
            },
            ExitCode::INTERNAL,
            "SERVER",
        ),
        (
            CliError::Partial {
                message: "2 of 3 ok".into(),
                succeeded: 2,
                failed: 1,
            },
            ExitCode::PARTIAL,
            "PARTIAL_FAILURE",
        ),
        (
            CliError::Internal("unexpected".into()),
            ExitCode::INTERNAL,
            "INTERNAL",
        ),
    ];
    for (err, expected_exit, expected_code) in cases {
        assert_eq!(
            err.exit_code(),
            expected_exit,
            "variant {err:?} should map to exit {expected_exit:?}"
        );
        assert_eq!(
            err.code(),
            expected_code,
            "variant {err:?} should expose code string {expected_code:?}"
        );
    }
}

#[test]
fn user_error_inner_code_wins_over_generic() {
    // The `User` variant carries its own discriminating code so agents
    // can branch on the specific failure (e.g. MARK_INVALID_RUN_ID,
    // EXPORT_TARGET_EXISTS) instead of the generic USER_ERROR umbrella.
    // Pin the inner-code-wins contract.
    let err = CliError::User {
        code: "MARK_INVALID_RUN_ID".into(),
        message: "not a UUID".into(),
        hint: Some("ids look like ...".into()),
    };
    assert_eq!(err.code(), "MARK_INVALID_RUN_ID");
    // Empty inner code falls back to the generic name.
    let fallback = CliError::User {
        code: "".into(),
        message: "...".into(),
        hint: None,
    };
    assert_eq!(fallback.code(), "USER_ERROR");
}

#[test]
fn auth_required_carries_actionable_hint() {
    // The CLI's most common failure mode. The hint is what tells the
    // user (or the agent rendering the error) to run `knack auth login`
    // — without it, the error message is just "not signed in" with no
    // direction.
    let hint = CliError::AuthRequired.hint();
    assert!(hint.is_some());
    let hint = hint.unwrap();
    assert!(
        hint.contains("login") || hint.contains("auth"),
        "AuthRequired hint should mention login/auth, got: {hint}"
    );
}

#[test]
fn partial_preserves_counts_in_envelope() {
    // The Partial variant is the load-bearing piece of bulk-mark's
    // contract. Agents key on succeeded/failed to decide whether to
    // re-issue the failed subset.
    let err = CliError::Partial {
        message: "9 of 10 marks succeeded; 1 failed".into(),
        succeeded: 9,
        failed: 1,
    };
    assert_eq!(err.exit_code(), ExitCode::PARTIAL);
    let env = err_envelope(&err);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "PARTIAL_FAILURE");
    // The variant body itself doesn't render the counters into the
    // envelope (the command-level emit_ok writes them into `data`);
    // what the envelope MUST preserve is the message + code, which is
    // what a downstream CI script needs to disambiguate from INTERNAL.
    assert!(env["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("9 of 10"));
}

// ── Envelope schema marker ─────────────────────────────────────────────

#[test]
fn json_envelopes_carry_schema_marker_v1() {
    // Agents key on `$schema` to detect breaking changes between
    // major CLI versions. Anything that emits an envelope must include
    // it — the constant is the single source of truth.
    assert_eq!(SCHEMA, "knack://cli/v1");
    let ok = ok_envelope(json!({"x": 1}));
    assert_eq!(ok["$schema"], SCHEMA);
    assert_eq!(ok["ok"], true);
    let err = err_envelope(&CliError::AuthRequired);
    assert_eq!(err["$schema"], SCHEMA);
    assert_eq!(err["ok"], false);
}

// ── Wiremock-driven contract smoke (the v0.7.x server-side surface) ─────

#[tokio::test]
async fn overview_response_parses_regression_and_stale_blocks() {
    // /runs/overview is the only place the regression flag is computed.
    // The CLI's deserializer drops silently if a field is renamed in
    // the response — this test asserts the shape the API actually emits
    // in v0.7.x, so a server-side rename surfaces here before the
    // human-visible regression UI silently goes blank.
    use knack_cli::api::overview as api_overview;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/runs/overview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skills": [
                {
                    "slug": "regressing",
                    "current_version": "0.2.0",
                    "runs_total": 14,
                    "succeeded": 10,
                    "failed": 4,
                    "success_rate": 0.714,
                    "p50_ms": 220,
                    "p95_ms": 1100,
                    "last_run_at": "2026-05-30T00:00:00Z",
                    "regression": {
                        "current_version": "0.2.0",
                        "prior_version": "0.1.9",
                        "delta_success_rate": -0.21,
                        "current_success_rate": 0.714,
                        "prior_success_rate": 0.924
                    },
                    "stale": false
                },
                {
                    "slug": "unused",
                    "current_version": null,
                    "runs_total": 0,
                    "succeeded": 0,
                    "failed": 0,
                    "success_rate": null,
                    "p50_ms": null,
                    "p95_ms": null,
                    "last_run_at": null,
                    "regression": null,
                    "stale": true
                }
            ]
        })))
        .mount(&server)
        .await;

    let resp = api_overview::get_overview(&client, &api_overview::OverviewQuery::default())
        .await
        .unwrap();
    assert_eq!(resp.skills.len(), 2);
    let reg = resp.skills.iter().find(|s| s.slug == "regressing").unwrap();
    let r = reg.regression.as_ref().expect("regression block should parse");
    assert_eq!(r.prior_version.as_str(), "0.1.9");
    assert!(r.delta_success_rate < 0.0);
    let stale = resp.skills.iter().find(|s| s.slug == "unused").unwrap();
    assert!(stale.stale);
    assert!(stale.last_run_at.is_none());
}

#[tokio::test]
async fn server_500_maps_to_internal_exit() {
    // Any 5xx from the API → INTERNAL exit (70), distinct from a 4xx
    // (USER) or a network failure (NETWORK). CI scripts rely on this
    // axis to decide retry vs bail.
    use knack_cli::api::skills as api_skills;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "ok": false,
            "error": {"code": "INTERNAL", "message": "boom"}
        })))
        .mount(&server)
        .await;

    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    assert!(matches!(err, CliError::Server { .. }), "got {err:?}");
    assert_eq!(err.exit_code(), ExitCode::INTERNAL);
}

#[tokio::test]
async fn plan_limit_403_maps_to_plan_exit() {
    // The free-tier cap surfaces as 403 + PLAN_LIMIT_EXCEEDED. The CLI
    // routes this to its own exit code (5) so a CI script can recognize
    // "this user is rate-limited / over quota" vs an auth issue (2) or
    // a write conflict (4). v0.7.x stabilized the code string; pin it.
    use knack_cli::api::skills as api_skills;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "ok": false,
            "error": {
                "code": "PLAN_LIMIT_EXCEEDED",
                "message": "free plan: 5 skills max",
                "hint": "upgrade at getknack.ai/pricing"
            }
        })))
        .mount(&server)
        .await;

    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    assert!(matches!(err, CliError::PlanLimit { .. }), "got {err:?}");
    assert_eq!(err.exit_code(), ExitCode::PLAN);
    assert_eq!(err.code(), "PLAN_LIMIT_EXCEEDED");
    // NOTE: the server-supplied hint isn't currently threaded into the
    // PlanLimit variant (map_api_error builds it with `hint: None`).
    // When/if that wiring lands, flip this to assert is_some().
}

#[tokio::test]
async fn pat_credential_does_not_attempt_refresh_on_401() {
    // PATs have no refresh semantics. v0.7.x re-confirmed this contract
    // (auth.rs covers the in-depth case); the smoke check here is "if
    // we accidentally remove the PAT-skip branch from the 401 retry
    // path, every PAT user starts seeing a phantom /auth/refresh call
    // that ALWAYS fails with no useful error."
    use knack_cli::api::skills as api_skills;
    let (server, client, _store) = common::fixture_pat().await;

    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": {"code": "AUTH_REQUIRED", "message": "revoked"}
        })))
        .mount(&server)
        .await;
    // NB: no /auth/refresh mock — if we hit it, wiremock's no-match
    // behavior produces a different error than AuthFailed, surfacing
    // the regression cleanly.
    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    assert!(
        matches!(err, CliError::AuthFailed(_)),
        "PAT 401 should map to AuthFailed without /auth/refresh attempt, got {err:?}"
    );
    assert_eq!(err.exit_code(), ExitCode::AUTH);
}

#[tokio::test]
async fn cli_token_create_then_revoke_round_trip() {
    // The CLI's `knack auth status` / `knack auth logout` flows depend
    // on these two endpoints. v0.7.x added the `scopes` field on
    // create; the smoke check is the round-trip parse, not the scope
    // semantics (those have their own integration suite).
    use knack_cli::api::auth as api_auth;
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/me/cli-tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "tok_smoke",
            "name": "knack-cli@smoke",
            "plaintext": "knack_pat_smoke_abcdef0123456789abcdef0123456789abcdef0123_xyz",
            "prefix": "knack_pat_smoke_a",
            "created_at": "2026-06-01T00:00:00Z",
            "expires_at": null
        })))
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/me/cli-tokens/tok_smoke"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let created = api_auth::create_cli_token(
        &client,
        "knack-cli@smoke",
        None,
        true,
        vec!["full".into()],
    )
    .await
    .unwrap();
    assert_eq!(created.id, "tok_smoke");
    assert!(created.plaintext.starts_with("knack_pat_"));
    api_auth::revoke_cli_token(&client, "tok_smoke").await.unwrap();
}

#[tokio::test]
async fn runs_by_skill_404_maps_to_not_found_user_exit() {
    // `knack runs list --skill=<bad-slug>` should exit 1 (USER), not 70.
    use knack_cli::api::runs as api_runs;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/runs/by-skill/no-such-skill"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": {"code": "NOT_FOUND", "message": "no such skill"}
        })))
        .mount(&server)
        .await;
    let err = api_runs::list_for_skill(
        &client,
        "no-such-skill",
        &api_runs::RunsListQuery::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CliError::NotFound(_)), "got {err:?}");
    assert_eq!(err.exit_code(), ExitCode::USER);
}

#[tokio::test]
async fn network_error_maps_to_network_exit() {
    // Pre-flight smoke: when the server is unreachable (wiremock not
    // started — we just point at an unused port), the CLI surfaces
    // NETWORK (3) rather than INTERNAL (70). Distinct exit codes let
    // CI scripts retry transient network issues without retrying
    // genuine logic bugs.
    use knack_cli::api::skills as api_skills;
    use knack_cli::auth_store::MemoryStore;
    use knack_cli::auth_store::TokenStore;
    use knack_cli::config::Config;
    use std::sync::Arc;

    let store = Arc::new(MemoryStore::new());
    store
        .save(
            "default",
            &knack_cli::auth_store::StoredCredential {
                token: "x".into(),
                token_id: None,
                prefix: None,
                refresh_token: None,
                expires_at: None,
                label: None,
                user_id: None,
                email: None,
            },
        )
        .unwrap();
    let mut config = Config::load();
    // 127.0.0.1:1 is reserved + reliably refused on every platform.
    config.api_base = "http://127.0.0.1:1".into();
    let client = knack_cli::api::ApiClient::new(
        config,
        store as Arc<dyn TokenStore + Send + Sync>,
        "default",
    );

    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    assert!(
        matches!(err, CliError::Network(_)),
        "unreachable host should surface as Network, got {err:?}"
    );
    assert_eq!(err.exit_code(), ExitCode::NETWORK);
}
