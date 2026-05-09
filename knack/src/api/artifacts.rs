//! Artifacts API — presign-upload, finalize. Used by `knack interview --local`
//! when the user runs `/upload <role> <path>` during the artifacts phase.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Serialize)]
pub struct PresignUploadRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_version_id: Option<String>,
    pub kind: String, // "input" | "output" | "example" | "test"
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PresignUploadResponse {
    pub artifact_id: String,
    pub upload_url: String,
    pub s3_key: String,
    #[allow(dead_code)]
    pub expires_in: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactFinalize {
    pub sha256: String, // 64 lowercase hex chars
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactRead {
    pub id: String,
    #[allow(dead_code)]
    pub skill_id: Option<String>,
    #[allow(dead_code)]
    pub kind: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

pub async fn presign_upload(
    client: &ApiClient,
    body: &PresignUploadRequest,
) -> Result<PresignUploadResponse, CliError> {
    let body = serde_json::to_value(body)?;
    client
        .send_json::<PresignUploadResponse>(|c| {
            Ok(c.request(Method::POST, "/artifacts/presign-upload")?
                .json(&body))
        })
        .await
}

pub async fn finalize(
    client: &ApiClient,
    artifact_id: &str,
    body: &ArtifactFinalize,
) -> Result<ArtifactRead, CliError> {
    let path = format!("/artifacts/{artifact_id}/finalize");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<ArtifactRead>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

/// PUT bytes to the presigned URL — the request bypasses the API entirely.
/// Returns the response status so callers can surface 5xx from R2 distinctly
/// from API failures.
pub async fn put_bytes_to_presigned(
    client: &ApiClient,
    upload_url: &str,
    body: bytes::Bytes,
    content_type: &str,
) -> Result<u16, CliError> {
    let resp = client
        .http
        .put(upload_url)
        .header("Content-Type", content_type)
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CliError::Server {
            status: status.as_u16(),
            code: "R2_UPLOAD_FAILED".into(),
            message: text,
        });
    }
    Ok(status.as_u16())
}
