//! Integration tests for `api::artifacts` against a wiremock server.

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::api::artifacts as api_art;

mod common;

#[tokio::test]
async fn presign_upload_round_trip() {
    let (server, client, _store) = common::fixture().await;

    let presign_url = format!("{}/r2/PUT-target", server.uri());

    Mock::given(method("POST"))
        .and(path("/artifacts/presign-upload"))
        .and(body_partial_json(json!({
            "skill_id": "sk1",
            "kind": "input",
            "filename": "receipts.xlsx",
            "size_bytes": 1024,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "artifact_id": "art-1",
            "upload_url": presign_url,
            "s3_key": "artifacts/sk1/draft/art-1-receipts.xlsx",
            "expires_in": 900,
        })))
        .mount(&server)
        .await;

    let resp = api_art::presign_upload(
        &client,
        &api_art::PresignUploadRequest {
            skill_id: Some("sk1".into()),
            skill_version_id: None,
            kind: "input".into(),
            filename: "receipts.xlsx".into(),
            size_bytes: 1024,
        },
    )
    .await
    .unwrap();
    assert_eq!(resp.artifact_id, "art-1");
    assert_eq!(resp.expires_in, 900);
    assert!(resp.upload_url.ends_with("/r2/PUT-target"));
}

#[tokio::test]
async fn put_bytes_to_presigned_succeeds_on_2xx() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("PUT"))
        .and(path("/r2/upload-here"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/r2/upload-here", server.uri());
    let status = api_art::put_bytes_to_presigned(
        &client,
        &url,
        bytes::Bytes::from_static(b"some bytes"),
        "application/octet-stream",
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
}

#[tokio::test]
async fn put_bytes_to_presigned_surfaces_5xx_from_r2() {
    use knack_cli::errors::CliError;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("PUT"))
        .and(path("/r2/sad"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream went away"))
        .mount(&server)
        .await;

    let url = format!("{}/r2/sad", server.uri());
    let err = api_art::put_bytes_to_presigned(
        &client,
        &url,
        bytes::Bytes::from_static(b"x"),
        "text/plain",
    )
    .await
    .unwrap_err();
    if let CliError::Server { status, code, .. } = err {
        assert_eq!(status, 503);
        assert_eq!(code, "R2_UPLOAD_FAILED");
    } else {
        panic!("expected Server error, got {err:?}");
    }
}

#[tokio::test]
async fn finalize_records_sha256() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/artifacts/art-1/finalize"))
        .and(body_partial_json(json!({
            "sha256": "a".repeat(64),
            "size_bytes": 42,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "art-1",
            "skill_id": "sk1",
            "skill_version_id": null,
            "kind": "input",
            "filename": "receipts.xlsx",
            "size_bytes": 42,
            "sha256": "a".repeat(64),
            "created_at": "2026-05-08T00:00:00Z",
        })))
        .mount(&server)
        .await;

    let r = api_art::finalize(
        &client,
        "art-1",
        &api_art::ArtifactFinalize {
            sha256: "a".repeat(64),
            size_bytes: 42,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.id, "art-1");
    assert_eq!(r.size_bytes, 42);
}
