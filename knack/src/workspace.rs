//! Workspace-local layout discovery.
//!
//! Knack treats the agent's project as the unit of organization: every
//! workspace gets a `.knack/` directory that holds pulled skills (read
//! side) and in-progress drafts (write side). Multiple agents working
//! in different repos on the same machine don't share state.
//!
//! Layout:
//!
//! ```text
//! <workspace>/.knack/
//! ├── .gitignore           # opt-in commits; everything else local-only
//! ├── README.md            # explains the layout to humans
//! ├── skills/              # `knack pull` writes here
//! │   └── <slug>/
//! └── drafts/              # `knack create` writes here
//!     └── <slug>/
//! ```
//!
//! Discovery walks up the tree git-style: `.knack/` in any ancestor
//! wins. If nothing is found we create one in CWD on first write so
//! the agent doesn't need to remember to `knack init` first. A
//! `--global` flag (or `KNACK_SKILLS_DIR` env) opts back into the
//! HOME-shared `~/.knack/skills/` pool for users who prefer that.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const WORKSPACE_DIR_NAME: &str = ".knack";
pub const SKILLS_SUBDIR: &str = "skills";
pub const DRAFTS_SUBDIR: &str = "drafts";
const README_FILE: &str = "README.md";
const GITIGNORE_FILE: &str = ".gitignore";

// ─── workspace discovery ────────────────────────────────────────────────────

/// Walk up the directory tree from `start` looking for an existing
/// ``.knack/`` directory. Returns the path TO that directory (not its
/// parent) or ``None`` when we hit the filesystem root without finding
/// one.
///
/// Symmetric with `git rev-parse --show-toplevel` in spirit — a single
/// `.knack/` checkpoint anywhere up the tree wins.
pub fn discover_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.canonicalize().ok().unwrap_or_else(|| start.to_path_buf());
    loop {
        let candidate = cur.join(WORKSPACE_DIR_NAME);
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Resolve where ``knack pull`` should write a fetched skill.
///
/// Priority order, first hit wins:
///
///   1. ``target`` — caller passed an explicit ``--target`` path.
///   2. ``home_fallback`` — caller passed ``--global`` (or
///      ``KNACK_SKILLS_DIR`` was set, which `Config::load` already
///      collapsed into ``home_fallback`` for us).
///   3. Nearest workspace's ``.knack/skills/`` walking up from ``cwd``.
///   4. ``cwd/.knack/skills/`` — create-on-write default. The agent
///      doesn't need to `knack init` first.
pub fn resolve_skills_root(
    cwd: &Path,
    global: bool,
    target: Option<&Path>,
    home_fallback: &Path,
) -> PathBuf {
    if let Some(t) = target {
        return t.to_path_buf();
    }
    if global {
        return home_fallback.to_path_buf();
    }
    if let Some(ws) = discover_workspace_root(cwd) {
        return ws.join(SKILLS_SUBDIR);
    }
    cwd.join(WORKSPACE_DIR_NAME).join(SKILLS_SUBDIR)
}

/// Same resolution rule as ``resolve_skills_root`` but for the drafts
/// directory (``knack create`` authoring scratchpad).
///
/// Global fallback lives at ``<home>/.knack/drafts/`` — same parent as
/// the global skills pool. Most users will never use this — drafts are
/// inherently per-project — but we keep the symmetry so the flag works
/// uniformly across commands.
pub fn resolve_drafts_root(
    cwd: &Path,
    global: bool,
    target: Option<&Path>,
    home_fallback: &Path,
) -> PathBuf {
    if let Some(t) = target {
        return t.to_path_buf();
    }
    if global {
        // home_fallback is "<home>/.knack/skills/" — peel the leaf and
        // attach "drafts/" so global drafts land under "<home>/.knack/".
        let parent = home_fallback.parent().unwrap_or(home_fallback);
        return parent.join(DRAFTS_SUBDIR);
    }
    if let Some(ws) = discover_workspace_root(cwd) {
        return ws.join(DRAFTS_SUBDIR);
    }
    cwd.join(WORKSPACE_DIR_NAME).join(DRAFTS_SUBDIR)
}

/// Find an existing skill folder by ``slug`` — checks drafts first
/// (likeliest source when publishing a new version), then skills (when
/// re-publishing a fork of a pulled skill), then the legacy HOME pool.
///
/// Used by ``knack publish``'s ``--from`` default so the agent rarely
/// has to spell the path.
pub fn resolve_existing_skill_dir(
    slug: &str,
    cwd: &Path,
    home_fallback: &Path,
) -> Option<PathBuf> {
    if let Some(ws) = discover_workspace_root(cwd) {
        for sub in [DRAFTS_SUBDIR, SKILLS_SUBDIR] {
            let candidate = ws.join(sub).join(slug);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    let home = home_fallback.join(slug);
    if home.is_dir() {
        return Some(home);
    }
    None
}

// ─── init scaffolding ──────────────────────────────────────────────────────

const GITIGNORE_TEMPLATE: &str = "\
# Workspace-local agent skills. Pulled skills + drafts are rebuildable
# via `knack pull` / `knack publish`, so we keep them out of source
# control by default. Remove individual entries from this file to commit
# specific skills (and their drafts) that you want pinned to the repo.
*
!.gitignore
!README.md
";

const README_TEMPLATE: &str = "\
# .knack/ — workspace agent skills

This directory is managed by the [knack](https://getknack.ai) CLI.

```
skills/   pulled skills (consume here)
drafts/   in-progress skill authoring
```

Common commands:

```
knack pull @author/slug      # add a skill to skills/
knack create my-slug --name \"Display Name\"
                              # scaffold a draft under drafts/my-slug/
knack publish my-slug        # push drafts/my-slug/ as a new version
```

By default `.gitignore` ignores everything in this folder — skills are
rebuildable from the cloud. Comment out entries to pin specific skills
to the repo.
";

/// Create a fresh ``.knack/`` workspace at ``at``. Idempotent — re-runs
/// just re-create any missing subdirs and never overwrite the README /
/// gitignore. Returns the path to the ``.knack/`` directory.
pub fn init_workspace(at: &Path) -> io::Result<PathBuf> {
    let ws = at.join(WORKSPACE_DIR_NAME);
    fs::create_dir_all(ws.join(SKILLS_SUBDIR))?;
    fs::create_dir_all(ws.join(DRAFTS_SUBDIR))?;

    let gitignore = ws.join(GITIGNORE_FILE);
    if !gitignore.exists() {
        fs::write(&gitignore, GITIGNORE_TEMPLATE)?;
    }
    let readme = ws.join(README_FILE);
    if !readme.exists() {
        fs::write(&readme, README_TEMPLATE)?;
    }
    Ok(ws)
}

/// True iff the path looks like an existing ``.knack/`` workspace
/// directory (has at least the two canonical subdirs).
pub fn is_workspace(p: &Path) -> bool {
    p.is_dir() && p.join(SKILLS_SUBDIR).is_dir() && p.join(DRAFTS_SUBDIR).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::tempdir;

    #[test]
    fn discover_finds_nearest_dot_knack() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        init_workspace(root.path()).unwrap();

        let found = discover_workspace_root(&nested).unwrap();
        let expected = root.path().canonicalize().unwrap().join(WORKSPACE_DIR_NAME);
        assert_eq!(found, expected);
    }

    #[test]
    fn discover_returns_none_when_no_workspace_exists() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        assert!(discover_workspace_root(&nested).is_none());
    }

    #[test]
    fn resolve_skills_root_prefers_explicit_target() {
        let cwd = tempdir().unwrap();
        let target = PathBuf::from("/tmp/custom-target");
        let home = PathBuf::from("/home/jane/.knack/skills");
        let resolved =
            resolve_skills_root(cwd.path(), false, Some(&target), &home);
        assert_eq!(resolved, target);
    }

    #[test]
    fn resolve_skills_root_global_falls_back_to_home() {
        let cwd = tempdir().unwrap();
        let home = PathBuf::from("/home/jane/.knack/skills");
        let resolved = resolve_skills_root(cwd.path(), true, None, &home);
        assert_eq!(resolved, home);
    }

    #[test]
    fn resolve_skills_root_uses_discovered_workspace() {
        let root = tempdir().unwrap();
        init_workspace(root.path()).unwrap();
        let nested = root.path().join("a");
        fs::create_dir_all(&nested).unwrap();
        let home = PathBuf::from("/home/jane/.knack/skills");

        let resolved = resolve_skills_root(&nested, false, None, &home);
        let canonical_root = root.path().canonicalize().unwrap();
        assert_eq!(resolved, canonical_root.join(WORKSPACE_DIR_NAME).join(SKILLS_SUBDIR));
    }

    #[test]
    fn resolve_skills_root_falls_back_to_cwd_dot_knack() {
        let root = tempdir().unwrap();
        let home = PathBuf::from("/home/jane/.knack/skills");

        // No `.knack/` exists yet, so discovery fails and the fallback
        // path is just `<cwd>/.knack/skills` (not canonicalized).
        let resolved = resolve_skills_root(root.path(), false, None, &home);
        assert_eq!(
            resolved,
            root.path().join(WORKSPACE_DIR_NAME).join(SKILLS_SUBDIR),
        );
        let _ = env::current_dir();
    }

    #[test]
    fn init_workspace_is_idempotent() {
        let root = tempdir().unwrap();
        let first = init_workspace(root.path()).unwrap();
        let second = init_workspace(root.path()).unwrap();
        assert_eq!(first, second);
        assert!(first.join(GITIGNORE_FILE).is_file());
        assert!(first.join(README_FILE).is_file());
        assert!(is_workspace(&first));
    }

    #[test]
    fn resolve_existing_skill_dir_prefers_drafts() {
        let root = tempdir().unwrap();
        let ws = init_workspace(root.path()).unwrap();
        let drafts = ws.join(DRAFTS_SUBDIR).join("foo");
        let skills = ws.join(SKILLS_SUBDIR).join("foo");
        fs::create_dir_all(&drafts).unwrap();
        fs::create_dir_all(&skills).unwrap();

        let home = PathBuf::from("/nonexistent");
        let resolved =
            resolve_existing_skill_dir("foo", root.path(), &home).unwrap();
        // `discover_workspace_root` canonicalizes on Unix (no-op) and
        // emits a `\\?\` UNC-prefixed path on Windows. Compare against
        // the same canonical form so the test passes everywhere.
        let canonical_drafts = drafts.canonicalize().unwrap();
        assert_eq!(resolved.canonicalize().unwrap(), canonical_drafts);
    }
}
