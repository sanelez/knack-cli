//! Local pre-flight check for `knack validate` and `knack publish --dry-run`.
//!
//! Confirms the skill folder has the two files every Anthropic-format
//! skill needs (`SKILL.md` + `meta.knack.yaml`) and that they aren't
//! empty. Deeper schema validation runs server-side: `publish` round-
//! trips through `SKILL_FORMAT_INVALID`, which returns the same
//! `{path, message, code}` issue shape this module emits — so callers
//! handling one envelope handle both.
//!
//! Used to be a Rust port of the server's Python validators (~370
//! lines). Kept in lockstep was a maintenance cost without a real
//! win — the round-trip on `publish` already pays for full schema
//! checks. This is now just the offline existence gate.
//!
//! Output shape mirrors the server's `SKILL_FORMAT_INVALID` envelope:
//! `details: { issues: [{path, message, code}] }`.

use std::path::Path;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
    pub code: &'static str,
}

#[derive(Debug, Default)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn into_details(self) -> Value {
        json!({
            "issues": self.issues.iter().map(|i| json!({
                "path": i.path,
                "message": i.message,
                "code": i.code,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn summary(&self) -> String {
        if self.issues.is_empty() {
            return "ok".to_string();
        }
        self.issues
            .iter()
            .map(|i| {
                if i.path.is_empty() {
                    i.message.clone()
                } else {
                    format!("{}: {}", i.path, i.message)
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn push(&mut self, path: &str, message: impl Into<String>, code: &'static str) {
        self.issues.push(ValidationIssue {
            path: path.into(),
            message: message.into(),
            code,
        });
    }
}

/// Existence-and-non-empty gate for a skill folder. Real schema
/// validation runs server-side on publish.
pub fn validate_skill_folder(dir: &Path) -> ValidationReport {
    let mut report = ValidationReport::default();

    if !dir.is_dir() {
        report.push("", "skill folder does not exist or is not a directory", "DIR_MISSING");
        return report;
    }

    for required in ["SKILL.md", "meta.knack.yaml"] {
        let path = dir.join(required);
        match std::fs::read(&path) {
            Err(_) => report.push(required, format!("missing required file `{required}`"), "FILE_MISSING"),
            Ok(bytes) if bytes.iter().all(|b| b.is_ascii_whitespace()) => {
                report.push(required, format!("`{required}` is empty"), "FILE_EMPTY")
            }
            Ok(_) => {}
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn ok_when_both_files_present_and_nonempty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "# x").unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: x").unwrap();
        assert!(validate_skill_folder(dir.path()).is_ok());
    }

    #[test]
    fn flags_missing_dir() {
        let report = validate_skill_folder(Path::new("/no/such/dir/anywhere"));
        assert!(!report.is_ok());
        assert_eq!(report.issues[0].code, "DIR_MISSING");
    }

    #[test]
    fn flags_missing_skill_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: x").unwrap();
        let report = validate_skill_folder(dir.path());
        assert!(!report.is_ok());
        assert!(report.issues.iter().any(|i| i.path == "SKILL.md" && i.code == "FILE_MISSING"));
    }

    #[test]
    fn flags_empty_meta_yaml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "# x").unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "   \n\n  ").unwrap();
        let report = validate_skill_folder(dir.path());
        assert!(!report.is_ok());
        assert!(report.issues.iter().any(|i| i.path == "meta.knack.yaml" && i.code == "FILE_EMPTY"));
    }

    #[test]
    fn details_envelope_carries_issue_array() {
        let report = validate_skill_folder(Path::new("/no/such/dir"));
        let details = report.into_details();
        assert!(details["issues"].is_array());
        assert!(details["issues"][0]["code"].is_string());
    }
}
