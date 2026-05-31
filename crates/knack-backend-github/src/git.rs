//! Git remote/branch resolution for the self-host telemetry + publish paths.
//!
//! Hardcoding `origin/main` was hostile to:
//!   - repos with `master` as the default branch
//!   - fork workflows where `origin` is the fork and `upstream` is canonical
//!   - any repo with branch protection requiring PRs against a non-`main` branch
//!
//! Precedence:
//!   1. `KNACK_REMOTE_NAME` / `KNACK_REMOTE_BRANCH` env vars (operator override)
//!   2. `git symbolic-ref refs/remotes/<remote>/HEAD` for the default branch
//!   3. Hardcoded `origin/main` fallback (with a one-time stderr warning)

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    pub remote: String,
    pub branch: String,
}

impl RemoteTarget {
    pub fn fallback() -> Self {
        Self {
            remote: "origin".into(),
            branch: "main".into(),
        }
    }
}

/// Best-effort resolver. Never errors out the caller — falls back to
/// `origin/main` if every probe fails so a misconfigured repo doesn't
/// strand telemetry that already landed on disk.
pub fn resolve_remote(repo: &Path) -> RemoteTarget {
    let remote = resolve_remote_name(repo);
    let branch = resolve_default_branch(repo, &remote);
    RemoteTarget { remote, branch }
}

fn resolve_remote_name(repo: &Path) -> String {
    if let Ok(v) = std::env::var("KNACK_REMOTE_NAME") {
        if !v.is_empty() {
            return v;
        }
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let remotes: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if remotes.iter().any(|r| r == "origin") {
                return "origin".into();
            }
            if remotes.len() == 1 {
                return remotes.into_iter().next().unwrap();
            }
            "origin".into()
        }
        _ => "origin".into(),
    }
}

fn resolve_default_branch(repo: &Path, remote: &str) -> String {
    if let Ok(v) = std::env::var("KNACK_REMOTE_BRANCH") {
        if !v.is_empty() {
            return v;
        }
    }
    // Fast path: the symbolic ref is populated by `git clone` and `git
    // remote set-head`. Avoids spawning gh just to read what's already
    // recorded locally.
    let ref_name = format!("refs/remotes/{remote}/HEAD");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--short", &ref_name])
        .output();
    if let Ok(o) = output {
        if o.status.success() {
            let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // Output is `<remote>/<branch>`; strip the prefix.
            if let Some(stripped) = raw.strip_prefix(&format!("{remote}/")) {
                if !stripped.is_empty() {
                    return stripped.to_string();
                }
            }
        }
    }
    // Slow path: ask gh. Only used when symbolic-ref isn't set (fresh
    // clone before `git remote set-head` ran, or a custom remote).
    let owner_repo_output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", remote])
        .output();
    let owner_repo = match owner_repo_output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return "main".into(),
    };
    let slug = parse_github_owner_repo(&owner_repo);
    if let Some(slug) = slug {
        let gh = Command::new("gh")
            .args([
                "repo",
                "view",
                &slug,
                "--json",
                "defaultBranchRef",
                "-q",
                ".defaultBranchRef.name",
            ])
            .output();
        if let Ok(o) = gh {
            if o.status.success() {
                let name = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    "main".into()
}

fn parse_github_owner_repo(url: &str) -> Option<String> {
    // Accept SSH (`git@github.com:owner/repo.git`) and HTTPS
    // (`https://github.com/owner/repo[.git]`).
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return Some(rest.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        return Some(rest.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        return Some(rest.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https_url() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/foo/bar.git"),
            Some("foo/bar".into())
        );
        assert_eq!(
            parse_github_owner_repo("https://github.com/foo/bar"),
            Some("foo/bar".into())
        );
    }

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:foo/bar.git"),
            Some("foo/bar".into())
        );
    }

    #[test]
    fn parse_non_github_returns_none() {
        assert_eq!(parse_github_owner_repo("https://example.com/x"), None);
    }

    #[test]
    fn fallback_is_origin_main() {
        let t = RemoteTarget::fallback();
        assert_eq!(t.remote, "origin");
        assert_eq!(t.branch, "main");
    }
}
