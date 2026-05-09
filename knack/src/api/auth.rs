//! Auth API — device-authorization flow types and calls.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicePollRequest<'a> {
    pub device_code: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DevicePollResponse {
    pub status: PollStatus,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollStatus {
    AuthorizationPending,
    Approved,
    Denied,
    Expired,
    SlowDown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub plan: String,
    pub auth_method: String,
}

pub async fn device_start(client: &ApiClient) -> Result<DeviceAuthStart, CliError> {
    client
        .send_json::<DeviceAuthStart>(|c| Ok(c.request_unauth(Method::POST, "/auth/device/start")))
        .await
}

pub async fn device_poll(
    client: &ApiClient,
    device_code: &str,
) -> Result<DevicePollResponse, CliError> {
    let body = DevicePollRequest { device_code };
    let body = serde_json::to_value(&body)?;
    client
        .send_json::<DevicePollResponse>(|c| {
            Ok(c.request_unauth(Method::POST, "/auth/device/poll")
                .json(&body))
        })
        .await
}

pub async fn me(client: &ApiClient) -> Result<Me, CliError> {
    client
        .send_json::<Me>(|c| c.request(Method::GET, "/auth/me"))
        .await
}

pub async fn logout(client: &ApiClient, refresh_token: Option<&str>) -> Result<(), CliError> {
    let body = serde_json::json!({ "refresh_token": refresh_token });
    client
        .send_empty(|c| Ok(c.request(Method::POST, "/auth/logout")?.json(&body)))
        .await
}
