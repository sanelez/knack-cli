//! Cross-skill portfolio overview. Wraps `GET /me/runs/overview`.
//!
//! Returns one [`SkillOverviewDto`] per skill the caller has read
//! access to, plus regression / staleness flags computed server-side.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Default)]
pub struct OverviewQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub min_runs: u64,
    /// Server-side filter: only return skills owned by this team. The
    /// caller must be a member of the team or the server 403s.
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverviewResponse {
    pub skills: Vec<SkillOverviewDto>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillOverviewDto {
    pub slug: String,
    pub current_version: Option<String>,
    pub runs_total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub success_rate: Option<f64>,
    #[serde(default)]
    pub p50_ms: Option<u64>,
    #[serde(default)]
    pub p95_ms: Option<u64>,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub regression: Option<RegressionInfoDto>,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegressionInfoDto {
    pub current_version: String,
    pub prior_version: String,
    pub delta_success_rate: f64,
    pub current_success_rate: Option<f64>,
    pub prior_success_rate: Option<f64>,
}

pub async fn get_overview(
    client: &ApiClient,
    q: &OverviewQuery,
) -> Result<OverviewResponse, CliError> {
    let q = q.clone();
    client
        .send_json::<OverviewResponse>(|c| {
            // `/runs/overview` lives next to `/runs/by-skill/{id}`. Both
            // are implicitly scoped to the caller via the auth bearer;
            // the server filters skill rows by `can_user_access`.
            let mut rb = c.request(Method::GET, "/runs/overview")?;
            rb = rb.query(&[("min_runs", q.min_runs.to_string())]);
            if let Some(s) = &q.since {
                rb = rb.query(&[("since", s.to_rfc3339())]);
            }
            if let Some(u) = &q.until {
                rb = rb.query(&[("until", u.to_rfc3339())]);
            }
            if let Some(tid) = &q.team_id {
                rb = rb.query(&[("team_id", tid.as_str())]);
            }
            Ok(rb)
        })
        .await
}
