//! HTTP client for the Knack API.
//!
//! Wraps `reqwest::Client` with three responsibilities:
//!  - Inject `Authorization: Bearer <access>` from the [`TokenStore`]
//!  - Translate `{ ok: false, error: { code, message } }` envelopes into [`CliError`]
//!  - Refresh tokens transparently on a single 401, re-attempting once
//!
//! Lower-level resource modules (`auth`, `skills`, `runs`) take a reference to
//! this client.

pub mod auth;
pub mod runs;
pub mod skills;
pub mod users;

use std::sync::Arc;

use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth_store::{StoredTokens, TokenStore};
use crate::config::Config;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
struct ApiErrorBody {
    error: ApiErrorObject,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiErrorObject {
    code: String,
    message: String,
    #[serde(default)]
    details: Option<Value>,
}

/// Try every shape we recognize, falling back to raw text.
///
/// Production servers wrap errors in `{"error": {"code", "message"}}`, but
/// FastAPI's built-in Pydantic 422s return `{"detail": [...]}`, and proxies
/// or rate-limiters can return plain text. Whatever we find, surface it to
/// the user instead of swallowing it as "no envelope".
fn map_api_error_bytes(status: StatusCode, bytes: Option<&[u8]>) -> CliError {
    let Some(bytes) = bytes else {
        return map_api_error(status, None);
    };
    if let Ok(env) = serde_json::from_slice::<ApiErrorBody>(bytes) {
        return map_api_error(status, Some(env));
    }
    if let Ok(detail) = serde_json::from_slice::<FastApiValidationBody>(bytes) {
        let msg = detail.format();
        return map_api_error(
            status,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "VALIDATION_ERROR".into(),
                    message: msg,
                    details: None,
                },
            }),
        );
    }
    let text = std::str::from_utf8(bytes).unwrap_or("<binary body>").trim();
    let snippet = if text.len() > 500 { format!("{}…", &text[..500]) } else { text.to_string() };
    map_api_error(
        status,
        Some(ApiErrorBody {
            error: ApiErrorObject {
                code: "UNKNOWN".into(),
                message: if snippet.is_empty() {
                    format!("server returned {status} with empty body")
                } else {
                    format!("server returned {status}: {snippet}")
                },
                details: None,
            },
        }),
    )
}

/// Render a `details` value into a short user-facing summary. Recognizes the
/// `{"issues": [{"path", "message"}]}` shape produced by SKILL_FORMAT_INVALID
/// and VALIDATION_ERROR; falls back to compact JSON for anything else.
fn format_details(details: Option<&Value>) -> Option<String> {
    let details = details?;
    if let Some(issues) = details.get("issues").and_then(|v| v.as_array()) {
        if issues.is_empty() {
            return None;
        }
        let parts: Vec<String> = issues
            .iter()
            .filter_map(|i| {
                let path = i.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let msg = i.get("message").and_then(|v| v.as_str()).unwrap_or("");
                if path.is_empty() && msg.is_empty() {
                    None
                } else if path.is_empty() {
                    Some(msg.to_string())
                } else {
                    Some(format!("{path}: {msg}"))
                }
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        return Some(format!("issues: {}", parts.join("; ")));
    }
    None
}

#[derive(Debug, Deserialize)]
struct FastApiValidationBody {
    detail: Vec<FastApiValidationItem>,
}

#[derive(Debug, Deserialize)]
struct FastApiValidationItem {
    #[serde(default)]
    loc: Vec<Value>,
    #[serde(default)]
    msg: String,
    #[serde(default, rename = "type")]
    kind: String,
}

impl FastApiValidationBody {
    fn format(&self) -> String {
        let parts: Vec<String> = self
            .detail
            .iter()
            .map(|i| {
                let loc = i
                    .loc
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if i.kind.is_empty() {
                    format!("{loc}: {}", i.msg)
                } else {
                    format!("{loc}: {} ({})", i.msg, i.kind)
                }
            })
            .collect();
        if parts.is_empty() {
            "request validation failed".into()
        } else {
            format!("request validation failed. {}", parts.join("; "))
        }
    }
}

/// Translate a status + JSON body into the right [`CliError`] variant.
fn map_api_error(status: StatusCode, body: Option<ApiErrorBody>) -> CliError {
    let (code, message) = match body {
        Some(b) => {
            let mut msg = b.error.message;
            // Fold structured details (e.g. SKILL_FORMAT_INVALID issues) into
            // the message so the user sees the specific fields that failed
            // instead of a generic "skill format validation failed".
            if let Some(extra) = format_details(b.error.details.as_ref()) {
                msg = format!("{msg}. {extra}");
            }
            (b.error.code, msg)
        }
        None => (
            "UNKNOWN".to_string(),
            format!("server returned {status} with no envelope"),
        ),
    };
    match (status.as_u16(), code.as_str()) {
        (401, _) => CliError::AuthFailed(message),
        (403, "PLAN_LIMIT_EXCEEDED") => CliError::PlanLimit {
            message,
            hint: None,
        },
        (403, _) => CliError::User {
            code: "FORBIDDEN".into(),
            message,
            hint: None,
        },
        (404, _) => CliError::NotFound(message),
        (409, _) => CliError::Conflict {
            message,
            hint: None,
        },
        (s, _) if s >= 500 => CliError::Server {
            status: s,
            code,
            message,
        },
        (s, _) => CliError::Server {
            status: s,
            code,
            message,
        },
    }
}

#[derive(Clone)]
pub struct ApiClient {
    pub config: Config,
    pub http: Client,
    pub store: Arc<dyn TokenStore + Send + Sync>,
    pub account: String,
    /// Optional override that bypasses the token store entirely (--auth-token).
    pub bearer_override: Option<String>,
}

impl ApiClient {
    pub fn new(
        config: Config,
        store: Arc<dyn TokenStore + Send + Sync>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            config,
            http: Client::builder()
                .user_agent(concat!("knack-cli/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client should build"),
            store,
            account: account.into(),
            bearer_override: None,
        }
    }

    pub fn with_bearer_override(mut self, token: Option<String>) -> Self {
        self.bearer_override = token;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.api_base, path)
    }

    fn current_access_token(&self) -> Result<Option<String>, CliError> {
        if let Some(t) = &self.bearer_override {
            return Ok(Some(t.clone()));
        }
        Ok(self.store.load(&self.account)?.map(|t| t.access_token))
    }

    /// Build an authenticated request. Returns the builder so callers can add
    /// JSON bodies, query params, etc.
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, CliError> {
        let mut rb = self.http.request(method, self.url(path));
        if let Some(tok) = self.current_access_token()? {
            rb = rb.bearer_auth(tok);
        }
        Ok(rb)
    }

    /// Build an unauthenticated request — for `/auth/device/start` and `/auth/device/poll`.
    pub fn request_unauth(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, self.url(path))
    }

    /// Send a request and decode JSON, mapping API error envelopes to typed errors.
    /// Retries once after refreshing tokens if the first attempt 401s.
    pub async fn send_json<T: for<'de> Deserialize<'de>>(
        &self,
        build: impl Fn(&Self) -> Result<RequestBuilder, CliError>,
    ) -> Result<T, CliError> {
        let resp = build(self)?.send().await?;
        let status = resp.status();
        if status.is_success() {
            return decode_body(resp).await;
        }
        // Refresh once, retry once.
        if status == StatusCode::UNAUTHORIZED
            && self.bearer_override.is_none()
            && self.try_refresh().await.is_ok()
        {
            let resp2 = build(self)?.send().await?;
            let status2 = resp2.status();
            if status2.is_success() {
                return decode_body(resp2).await;
            }
            let bytes2 = resp2.bytes().await.ok();
            return Err(map_api_error_bytes(status2, bytes2.as_deref()));
        }
        let bytes = resp.bytes().await.ok();
        Err(map_api_error_bytes(status, bytes.as_deref()))
    }

    /// Send a request and discard the body (204 / 200 with empty body).
    pub async fn send_empty(
        &self,
        build: impl Fn(&Self) -> Result<RequestBuilder, CliError>,
    ) -> Result<(), CliError> {
        let resp = build(self)?.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::UNAUTHORIZED
            && self.bearer_override.is_none()
            && self.try_refresh().await.is_ok()
        {
            let resp2 = build(self)?.send().await?;
            let status2 = resp2.status();
            if status2.is_success() {
                return Ok(());
            }
            let bytes2 = resp2.bytes().await.ok();
            return Err(map_api_error_bytes(status2, bytes2.as_deref()));
        }
        let bytes = resp.bytes().await.ok();
        Err(map_api_error_bytes(status, bytes.as_deref()))
    }

    /// Force a refresh, persist the new pair, and return the seconds until
    /// the new access token expires. Used by `knack auth refresh` so
    /// long-running agents can proactively roll the token before it expires.
    pub async fn refresh_tokens(&self) -> Result<i64, CliError> {
        self.try_refresh().await?;
        let stored = self
            .store
            .load(&self.account)?
            .ok_or(CliError::AuthRequired)?;
        Ok(stored.expires_at - chrono::Utc::now().timestamp())
    }

    async fn try_refresh(&self) -> Result<(), CliError> {
        let Some(stored) = self.store.load(&self.account)? else {
            return Err(CliError::AuthRequired);
        };
        let body = serde_json::json!({ "refresh_token": stored.refresh_token });
        let resp = self
            .http
            .post(self.url("/auth/refresh"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            // Refresh failed → wipe the stale tokens so the user is prompted.
            let _ = self.store.clear(&self.account);
            return Err(CliError::AuthRequired);
        }

        #[derive(Deserialize)]
        struct RefreshResp {
            access_token: String,
            refresh_token: String,
            expires_in: i64,
        }
        let r: RefreshResp = resp.json().await?;
        let new = StoredTokens {
            access_token: r.access_token,
            refresh_token: r.refresh_token,
            expires_at: chrono::Utc::now().timestamp() + r.expires_in,
        };
        self.store.save(&self.account, &new)?;
        Ok(())
    }
}

async fn decode_body<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T, CliError> {
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        // Some endpoints return 200 with empty body; let serde_json handle null
        // for option-typed fields by trying "null".
        return serde_json::from_slice::<T>(b"null").map_err(CliError::from);
    }
    serde_json::from_slice::<T>(&bytes).map_err(CliError::from)
}

/// Page wrapper matching `Page[T]` from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_401_to_auth_failed() {
        let err = map_api_error(
            StatusCode::UNAUTHORIZED,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "AUTH_REQUIRED".into(),
                    message: "no token".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::AuthFailed(_));
    }

    #[test]
    fn maps_403_plan_limit_to_plan_variant() {
        let err = map_api_error(
            StatusCode::FORBIDDEN,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "PLAN_LIMIT_EXCEEDED".into(),
                    message: "too many".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::PlanLimit { .. });
    }

    #[test]
    fn maps_404_to_not_found() {
        let err = map_api_error(
            StatusCode::NOT_FOUND,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "NOT_FOUND".into(),
                    message: "missing".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::NotFound(_));
    }

    #[test]
    fn maps_409_to_conflict() {
        let err = map_api_error(
            StatusCode::CONFLICT,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "IMMUTABLE_VERSION".into(),
                    message: "exists".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::Conflict { .. });
    }

    #[test]
    fn maps_5xx_to_server() {
        let err = map_api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "X".into(),
                    message: "boom".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::Server { .. });
    }
}
