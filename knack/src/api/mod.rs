//! HTTP client for the Knack API.
//!
//! Wraps `reqwest::Client` with three responsibilities:
//!  - Inject `Authorization: Bearer <access>` from the [`TokenStore`]
//!  - Translate `{ ok: false, error: { code, message } }` envelopes into [`CliError`]
//!  - Refresh tokens transparently on a single 401, re-attempting once
//!
//! Lower-level resource modules (`auth`, `skills`, `runs`, `interview`) take a
//! reference to this client.

pub mod artifacts;
pub mod auth;
pub mod interview;
pub mod runs;
pub mod skills;
pub mod sse;

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
    #[allow(dead_code)]
    details: Option<Value>,
}

/// Translate a status + JSON body into the right [`CliError`] variant.
fn map_api_error(status: StatusCode, body: Option<ApiErrorBody>) -> CliError {
    let (code, message) = match body {
        Some(b) => (b.error.code, b.error.message),
        None => (
            "UNKNOWN".to_string(),
            format!("server returned {status} with no envelope"),
        ),
    };
    match (status.as_u16(), code.as_str()) {
        (401, _) => CliError::AuthFailed(message),
        (403, "PLAN_LIMIT_EXCEEDED") => CliError::PlanLimit { message, hint: None },
        (403, _) => CliError::User {
            code: "FORBIDDEN".into(),
            message,
            hint: None,
        },
        (404, _) => CliError::NotFound(message),
        (409, _) => CliError::Conflict { message, hint: None },
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
        if status == StatusCode::UNAUTHORIZED && self.bearer_override.is_none() {
            // Refresh once, retry once.
            if self.try_refresh().await.is_ok() {
                let resp2 = build(self)?.send().await?;
                let status2 = resp2.status();
                if status2.is_success() {
                    return decode_body(resp2).await;
                }
                let body = resp2.json::<ApiErrorBody>().await.ok();
                return Err(map_api_error(status2, body));
            }
        }
        let body = resp.json::<ApiErrorBody>().await.ok();
        Err(map_api_error(status, body))
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
        if status == StatusCode::UNAUTHORIZED && self.bearer_override.is_none() {
            if self.try_refresh().await.is_ok() {
                let resp2 = build(self)?.send().await?;
                let status2 = resp2.status();
                if status2.is_success() {
                    return Ok(());
                }
                let body = resp2.json::<ApiErrorBody>().await.ok();
                return Err(map_api_error(status2, body));
            }
        }
        let body = resp.json::<ApiErrorBody>().await.ok();
        Err(map_api_error(status, body))
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
