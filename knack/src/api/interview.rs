//! Interview API — POST /interview/sessions, POST /answer (SSE), POST /compile (SSE).

use chrono::{DateTime, Utc};
use reqwest::{Method, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Serialize)]
pub struct SessionCreate {
    pub mode: String, // "web" | "cli" | "video"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starter_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRead {
    pub id: String,
    #[allow(dead_code)]
    pub user_id: String,
    pub mode: String,
    pub current_phase: String,
    pub state: Value,
    #[allow(dead_code)]
    pub skill_id: Option<String>,
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnswerBody {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_voice: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_skill_id: Option<String>,
    pub target_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactAttachBody {
    pub artifact_id: String,
    pub role: String, // "input" | "output" | "example"
    pub filename: String,
}

pub async fn attach_artifact(
    client: &ApiClient,
    session_id: &str,
    body: &ArtifactAttachBody,
) -> Result<SessionRead, CliError> {
    let path = format!("/interview/sessions/{session_id}/artifact");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<SessionRead>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

pub async fn create_session(
    client: &ApiClient,
    body: &SessionCreate,
) -> Result<SessionRead, CliError> {
    let body = serde_json::to_value(body)?;
    client
        .send_json::<SessionRead>(|c| {
            Ok(c.request(Method::POST, "/interview/sessions")?.json(&body))
        })
        .await
}

pub async fn get_session(client: &ApiClient, id: &str) -> Result<SessionRead, CliError> {
    let path = format!("/interview/sessions/{id}");
    client
        .send_json::<SessionRead>(|c| c.request(Method::GET, &path))
        .await
}

/// POST /interview/sessions/{id}/answer — returns the streaming Response so the
/// caller can wrap it in the SSE parser. Status checks happen inline because
/// the high-level `send_json` would consume the body.
pub async fn submit_answer_streaming(
    client: &ApiClient,
    session_id: &str,
    body: &AnswerBody,
) -> Result<Response, CliError> {
    let path = format!("/interview/sessions/{session_id}/answer");
    let body = serde_json::to_value(body)?;
    let resp = client
        .request(Method::POST, &path)?
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await?;
    expect_streaming(resp).await
}

pub async fn compile_streaming(
    client: &ApiClient,
    session_id: &str,
    body: &CompileBody,
) -> Result<Response, CliError> {
    let path = format!("/interview/sessions/{session_id}/compile");
    let body = serde_json::to_value(body)?;
    let resp = client
        .request(Method::POST, &path)?
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await?;
    expect_streaming(resp).await
}

async fn expect_streaming(resp: Response) -> Result<Response, CliError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CliError::AuthFailed("session expired".into()));
    }
    let text = resp.text().await.unwrap_or_default();
    Err(CliError::Server {
        status: status.as_u16(),
        code: "STREAM_OPEN_FAILED".into(),
        message: text,
    })
}
