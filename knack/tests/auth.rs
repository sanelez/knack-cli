//! Integration tests for the auth flows: device-grant happy path + 401
//! transparent refresh-and-retry.

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::api::auth as api_auth;
use knack_cli::api::skills as api_skills;

mod common;

#[tokio::test]
async fn device_start_returns_codes_and_uri() {
    let (server, client) = common::fixture_unauth().await;

    Mock::given(method("POST"))
        .and(path("/auth/device/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-abc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://getknack.ai/cli-auth?code=ABCD-1234",
            "expires_in": 600,
            "interval": 5,
        })))
        .mount(&server)
        .await;

    let s = api_auth::device_start(&client).await.unwrap();
    assert_eq!(s.device_code, "dc-abc");
    assert_eq!(s.user_code, "ABCD-1234");
    assert!(s.verification_uri.contains("ABCD-1234"));
    assert_eq!(s.interval, 5);
}

#[tokio::test]
async fn device_poll_pending_then_approved() {
    let (server, client) = common::fixture_unauth().await;

    // First poll: still pending. up_to_n_times(1) so only the first call
    // matches this mock; subsequent calls fall through to the approved one.
    Mock::given(method("POST"))
        .and(path("/auth/device/poll"))
        .and(body_partial_json(json!({"device_code": "dc-1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "authorization_pending",
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/auth/device/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "approved",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;

    let first = api_auth::device_poll(&client, "dc-1").await.unwrap();
    assert!(matches!(
        first.status,
        api_auth::PollStatus::AuthorizationPending
    ));

    let second = api_auth::device_poll(&client, "dc-1").await.unwrap();
    assert!(matches!(second.status, api_auth::PollStatus::Approved));
    assert_eq!(second.access_token.as_deref(), Some("new-access"));
}

#[tokio::test]
async fn refresh_on_401_retries_and_persists_new_tokens() {
    use knack_cli::auth_store::TokenStore;

    let (server, client, store) = common::fixture().await;

    // First /skills call: 401 (token went stale). up_to_n_times(1).
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": { "code": "AUTH_REQUIRED", "message": "expired" }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // /auth/refresh: hand back a fresh pair. We assert the *old* refresh
    // token was sent so the rotation path is exercised.
    Mock::given(method("POST"))
        .and(path("/auth/refresh"))
        .and(body_partial_json(json!({
            "refresh_token": common::FAKE_REFRESH_TOKEN,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "rotated-access",
            "refresh_token": "rotated-refresh",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;

    // Second /skills call (after refresh): success.
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "next_cursor": null,
        })))
        .mount(&server)
        .await;

    let page = api_skills::list(&client, None, None, 50).await.unwrap();
    assert!(page.items.is_empty());

    // The retry path should have written the new tokens to the store.
    let stored = store.load("default").unwrap().expect("tokens persisted");
    assert_eq!(stored.access_token, "rotated-access");
    assert_eq!(stored.refresh_token, "rotated-refresh");
}

#[tokio::test]
async fn refresh_failure_clears_store_and_surfaces_auth_required() {
    use knack_cli::auth_store::TokenStore;
    use knack_cli::errors::CliError;

    let (server, client, store) = common::fixture().await;

    // /skills always 401.
    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": { "code": "AUTH_REQUIRED", "message": "stale" }
        })))
        .mount(&server)
        .await;

    // /auth/refresh also fails — the user really is logged out.
    Mock::given(method("POST"))
        .and(path("/auth/refresh"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": { "code": "AUTH_REQUIRED", "message": "refresh expired" }
        })))
        .mount(&server)
        .await;

    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    // After the refresh fails the original 401 is surfaced via map_api_error.
    assert!(
        matches!(err, CliError::AuthFailed(_)),
        "expected AuthFailed, got {err:?}"
    );

    // And the stale tokens are wiped — the user should be prompted to re-login.
    assert!(store.load("default").unwrap().is_none());
}

#[tokio::test]
async fn me_endpoint_returns_user_info() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/auth/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "u1",
            "email": "jane@firm.com",
            "name": "Jane",
            "plan": "pro",
            "auth_method": "cli",
        })))
        .mount(&server)
        .await;

    let me = api_auth::me(&client).await.unwrap();
    assert_eq!(me.email, "jane@firm.com");
    assert_eq!(me.plan, "pro");
}
