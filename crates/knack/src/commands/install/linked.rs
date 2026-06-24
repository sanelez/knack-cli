//! Persistent record of which skills `knack link` has installed into
//! agent skill directories, and where.
//!
//! Mirrors [`super::installed`] in shape and storage discipline. The
//! registry is *bookkeeping*: it powers `knack link --list` and a future
//! relink-on-upgrade. It is NOT the source of truth for removal —
//! `knack unlink` walks the agent skill roots and removes sigil-protected
//! folders directly, so an out-of-band edit (a hand-deleted folder) never
//! leaves `unlink` stuck. We still keep the registry in lockstep on the
//! happy path so `--list` is accurate.
//!
//! Layout (JSON):
//!
//! ```json
//! {
//!   "version": 1,
//!   "skills": [
//!     {"slug": "monthly-close", "version": "1.2.0", "scope": "home",
//!      "agents": ["claude", "codex"]}
//!   ],
//!   "updated_at": "2026-06-23T14:32:18Z"
//! }
//! ```
//!
//! Storage location, in order:
//!   1. `$KNACK_LINKED_FILE` (env override — used by tests)
//!   2. `~/.knack/linked.json`
//!   3. fallback: `$XDG_CONFIG_HOME/knack/linked.json` when no home dir
//!      is visible (rare; matches the rest of the CLI)

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::installed::Scope;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedSkill {
    pub slug: String,
    pub version: String,
    pub scope: Scope,
    /// Agent target slugs (`targets::AgentTarget::name`) the skill was
    /// written to at link time.
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedRecord {
    pub version: u32,
    #[serde(default)]
    pub skills: Vec<LinkedSkill>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for LinkedRecord {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            skills: Vec::new(),
            updated_at: None,
        }
    }
}

/// Resolve the on-disk location for the registry.
pub fn record_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_LINKED_FILE") {
        return Some(PathBuf::from(custom));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".knack").join("linked.json"));
    }
    dirs::config_dir().map(|c| c.join("knack").join("linked.json"))
}

/// Read the registry. Missing file is normal (nothing linked yet) and
/// yields an empty record. Malformed JSON is surfaced rather than masked.
pub fn load() -> io::Result<LinkedRecord> {
    let Some(path) = record_path() else {
        return Ok(LinkedRecord::default());
    };
    match fs::read_to_string(&path) {
        Ok(s) => {
            let rec: LinkedRecord = serde_json::from_str(&s).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{path:?}: {e}"))
            })?;
            Ok(rec)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LinkedRecord::default()),
        Err(e) => Err(e),
    }
}

/// Persist atomically (tempfile + rename in the same directory).
pub fn save(rec: &LinkedRecord) -> io::Result<()> {
    let Some(path) = record_path() else {
        return Err(io::Error::other(
            "no home or config dir available for linked.json",
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut record = rec.clone();
    record.version = SCHEMA_VERSION;
    record.updated_at = Some(Utc::now());
    let body = serde_json::to_string_pretty(&record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Record (or replace) the entry for `slug` at `scope`. A skill linked at
/// both Home and Project scope keeps two entries — they live in different
/// agent directories and are removed independently.
pub fn add(slug: &str, version: &str, scope: Scope, agents: Vec<String>) -> io::Result<()> {
    let mut rec = load()?;
    rec.skills
        .retain(|s| !(s.slug == slug && s.scope == scope));
    rec.skills.push(LinkedSkill {
        slug: slug.to_string(),
        version: version.to_string(),
        scope,
        agents,
    });
    save(&rec)
}

/// Drop the entry for `slug` at `scope`. Returns true when one was removed.
pub fn remove(slug: &str, scope: Scope) -> io::Result<bool> {
    let mut rec = load()?;
    let before = rec.skills.len();
    rec.skills
        .retain(|s| !(s.slug == slug && s.scope == scope));
    let changed = rec.skills.len() != before;
    if changed {
        save(&rec)?;
    }
    Ok(changed)
}

/// All currently-recorded linked skills.
pub fn list() -> io::Result<Vec<LinkedSkill>> {
    Ok(load()?.skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn isolate() -> (MutexGuard<'static, ()>, TempDir, PathBuf) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("linked.json");
        // SAFETY: ENV_LOCK serializes every test that touches this var.
        unsafe {
            std::env::set_var("KNACK_LINKED_FILE", &path);
        }
        (guard, dir, path)
    }

    #[test]
    fn load_missing_returns_empty() {
        let (_g, _d, _p) = isolate();
        assert!(load().unwrap().skills.is_empty());
    }

    #[test]
    fn add_then_remove_round_trips() {
        let (_g, _d, path) = isolate();
        add("demo", "1.0.0", Scope::Home, vec!["claude".into()]).unwrap();
        assert!(path.exists());
        let entries = list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].slug, "demo");
        assert_eq!(entries[0].agents, vec!["claude".to_string()]);

        assert!(remove("demo", Scope::Home).unwrap());
        assert!(list().unwrap().is_empty());
        assert!(!remove("demo", Scope::Home).unwrap()); // idempotent
    }

    #[test]
    fn add_dedupes_same_slug_same_scope_keeps_both_scopes() {
        let (_g, _d, _p) = isolate();
        add("demo", "1.0.0", Scope::Home, vec!["claude".into()]).unwrap();
        add("demo", "1.1.0", Scope::Home, vec!["claude".into()]).unwrap();
        add("demo", "1.0.0", Scope::Project, vec!["codex".into()]).unwrap();
        let entries = list().unwrap();
        assert_eq!(entries.len(), 2);
        let home = entries.iter().find(|s| s.scope == Scope::Home).unwrap();
        assert_eq!(home.version, "1.1.0");
    }
}
