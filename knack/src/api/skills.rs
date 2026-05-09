//! Skills API — list, create, get, version CRUD.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, Page};
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Skill {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub owner_team_id: Option<String>,
    pub current_version_id: Option<String>,
    pub current_version_semver: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillVersion {
    pub id: String,
    pub skill_id: String,
    pub version: String,
    pub skill_md: String,
    pub intuition_md: String,
    pub meta_yaml: String,
    pub parent_version_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillCreate {
    pub slug: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_team_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillVersionCreate {
    pub version: String,
    pub skill_md: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intuition_md: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meta_yaml: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
}

pub async fn list(
    client: &ApiClient,
    scope: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Page<Skill>, CliError> {
    let scope = scope.map(str::to_string);
    let cursor = cursor.map(str::to_string);
    client
        .send_json::<Page<Skill>>(|c| {
            let mut rb = c.request(Method::GET, "/skills")?;
            rb = rb.query(&[("limit", limit.to_string())]);
            if let Some(s) = &scope {
                rb = rb.query(&[("scope", s)]);
            }
            if let Some(cur) = &cursor {
                rb = rb.query(&[("cursor", cur)]);
            }
            Ok(rb)
        })
        .await
}

pub async fn get(client: &ApiClient, skill_id: &str) -> Result<Skill, CliError> {
    let path = format!("/skills/{skill_id}");
    client
        .send_json::<Skill>(|c| c.request(Method::GET, &path))
        .await
}

/// Find a skill by slug. Falls back to scanning the user's accessible skills
/// since the API doesn't (yet) expose a slug→id endpoint. Cheap for free-tier
/// users (≤3 skills) and good-enough for v0.
pub async fn find_by_slug(client: &ApiClient, slug: &str) -> Result<Option<Skill>, CliError> {
    let mut cursor: Option<String> = None;
    loop {
        let page = list(client, None, cursor.as_deref(), 200).await?;
        for s in &page.items {
            if s.slug == slug {
                return Ok(Some(s.clone()));
            }
        }
        if page.next_cursor.is_none() {
            return Ok(None);
        }
        cursor = page.next_cursor;
    }
}

pub async fn get_version(
    client: &ApiClient,
    skill_id: &str,
    semver: &str,
) -> Result<SkillVersion, CliError> {
    let path = format!("/skills/{skill_id}/versions/{semver}");
    client
        .send_json::<SkillVersion>(|c| c.request(Method::GET, &path))
        .await
}

pub async fn list_versions(
    client: &ApiClient,
    skill_id: &str,
) -> Result<Page<SkillVersion>, CliError> {
    let path = format!("/skills/{skill_id}/versions");
    client
        .send_json::<Page<SkillVersion>>(|c| {
            Ok(c.request(Method::GET, &path)?.query(&[("limit", "200")]))
        })
        .await
}

pub async fn create(client: &ApiClient, body: &SkillCreate) -> Result<Skill, CliError> {
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Skill>(|c| Ok(c.request(Method::POST, "/skills")?.json(&body)))
        .await
}

pub async fn create_version(
    client: &ApiClient,
    skill_id: &str,
    body: &SkillVersionCreate,
) -> Result<SkillVersion, CliError> {
    let path = format!("/skills/{skill_id}/versions");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<SkillVersion>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}
