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

use serde::{Deserialize, Serialize};

const WORKSPACE_DIR_NAME: &str = ".knack";
pub const SKILLS_SUBDIR: &str = "skills";
pub const DRAFTS_SUBDIR: &str = "drafts";
const README_FILE: &str = "README.md";
const GITIGNORE_FILE: &str = ".gitignore";
pub const FOLDERS_INDEX_FILE: &str = "folders.json";

// ─── workspace discovery ────────────────────────────────────────────────────

/// Walk up the directory tree from `start` looking for an existing
/// ``.knack/`` directory. Returns the path TO that directory (not its
/// parent) or ``None`` when we hit either the filesystem root OR the
/// user's HOME without finding one.
///
/// The HOME boundary is load-bearing: the user's `~/.knack/` is the
/// `--global` pull pool, not a workspace root that every project under
/// HOME silently inherits. Without this guard, a fresh `knack create`
/// from any project dir on a typical setup
/// (`C:\Users\Jordan\PycharmProjects\…` or `~/code/…`) would resolve
/// to `~/.knack/skills/` and quietly pollute the global pool. Stop
/// the walk BEFORE inspecting HOME so the next caller falls back to
/// `<cwd>/.knack/skills/` (the project-local default) per
/// `resolve_skills_root` rule #4.
///
/// Symmetric with `git rev-parse --show-toplevel` in spirit — a single
/// `.knack/` checkpoint anywhere up the tree wins, bounded by HOME.
pub fn discover_workspace_root(start: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    discover_workspace_root_with_home(start, home.as_deref())
}

/// Inner form of [`discover_workspace_root`] that takes the HOME path
/// explicitly. Production code goes through `discover_workspace_root`
/// (which resolves HOME via `dirs::home_dir()`); tests use this form
/// directly with a controlled fake HOME because `dirs::home_dir()` on
/// Windows reads from the Win32 API rather than the `USERPROFILE` env
/// var, so an `env::set_var` shim in a test wouldn't take effect.
fn discover_workspace_root_with_home(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    // Canonicalize both paths so the comparison sees the same shape —
    // tempfile-style paths on Windows often gain a `\\?\` verbatim
    // prefix after canonicalize that the raw input doesn't have.
    let home_canon = home.and_then(|h| h.canonicalize().ok());
    let mut cur = start
        .canonicalize()
        .ok()
        .unwrap_or_else(|| start.to_path_buf());
    loop {
        if let Some(h) = &home_canon {
            if cur == *h {
                return None;
            }
        }
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
pub fn resolve_existing_skill_dir(slug: &str, cwd: &Path, home_fallback: &Path) -> Option<PathBuf> {
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

    // If `at/` is a git repo (self-host workspace or any project that
    // happens to be git-tracked), make sure the parent `.gitignore`
    // excludes the workspace dir. Without this, a self-host user who
    // ran `knack init` then `knack create` then `knack publish` would
    // hit `has_unrelated_dirty` in `knack-backend-github::backend.rs`,
    // which refuses to publish when the working tree has uncommitted
    // changes outside `skills/<slug>/`. The workspace `.knack/` is
    // local agent state (drafts, folders index) — never part of the
    // skill artifact contract.
    if at.join(".git").is_dir() {
        ensure_root_gitignore_excludes_workspace(at)?;
    }

    Ok(ws)
}

/// Append `.knack/` to `<repo>/.gitignore` if a) the file already
/// exists and doesn't mention the workspace, or b) the file doesn't
/// exist yet — in which case we create a minimal one. Idempotent and
/// non-destructive: hand-edited gitignores keep their content.
fn ensure_root_gitignore_excludes_workspace(repo_root: &Path) -> io::Result<()> {
    let gitignore = repo_root.join(GITIGNORE_FILE);
    let line = format!("{}/", WORKSPACE_DIR_NAME);
    if gitignore.exists() {
        let existing = fs::read_to_string(&gitignore)?;
        // Cheap textual match — a hand-written `.knack` or `.knack/` is
        // fine and we don't need to be strict about trailing slash.
        let already_excluded = existing
            .lines()
            .any(|raw| {
                let trimmed = raw.trim();
                trimmed == ".knack" || trimmed == ".knack/" || trimmed == "/.knack" || trimmed == "/.knack/"
            });
        if already_excluded {
            return Ok(());
        }
        let needs_newline = !existing.is_empty() && !existing.ends_with('\n');
        let mut updated = existing;
        if needs_newline {
            updated.push('\n');
        }
        updated.push_str(&line);
        updated.push('\n');
        fs::write(&gitignore, updated)?;
    } else {
        fs::write(&gitignore, format!("{}\n", line))?;
    }
    Ok(())
}

/// True iff the path looks like an existing ``.knack/`` workspace
/// directory (has at least the two canonical subdirs).
pub fn is_workspace(p: &Path) -> bool {
    p.is_dir() && p.join(SKILLS_SUBDIR).is_dir() && p.join(DRAFTS_SUBDIR).is_dir()
}

// ─── folders.json index ────────────────────────────────────────────────────
//
// Workspace-local cache of folder→slug assignments. The cloud is the
// source of truth; this index is the on-disk reflection so `knack list
// --folder=<name>` and `knack folder list` work offline-ish and so the
// next `knack pull` doesn't have to refetch every skill just to rebuild
// folder membership. Missing file is normal (no folders ever assigned in
// this workspace) and reads as an empty index.

/// One folder, with its current member slugs. ``scope`` mirrors the
/// server-side ``Folder.scope`` (personal vs team) so the CLI can
/// disambiguate without re-resolving the owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FolderIndexEntry {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub owner_team_id: Option<String>,
    pub slugs: Vec<String>,
}

/// Current on-disk schema version for `folders.json`. Bump when the
/// shape changes incompatibly so older / future CLIs can refuse to
/// silently corrupt the file. Migrations from older shapes happen in
/// `read_folders_index` — see the version-match arms there.
pub const FOLDERS_INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FoldersIndex {
    /// Schema version. Older shapes (no `version` field) read as `0`
    /// via serde default and trigger the "rebuild from cloud" fallback.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub folders: Vec<FolderIndexEntry>,
}

impl FoldersIndex {
    pub fn current() -> Self {
        Self {
            version: FOLDERS_INDEX_VERSION,
            folders: Vec::new(),
        }
    }
}

/// Read the workspace's folder index. Missing file returns an empty
/// index — folder assignment is a new feature, plenty of workspaces
/// won't have it yet. **Malformed** files return an empty index (with
/// a stderr warning); they'll be repopulated on the next `knack pull`
/// or folder write. We never want a corrupt cache file to brick the
/// whole CLI.
///
/// Version handling:
///
///   * `version == FOLDERS_INDEX_VERSION (1)` — current shape, return as-is.
///   * `version == 0` (legacy / pre-versioning files) — accept the
///     entries we can read, stamp the current version. The shape
///     hasn't actually changed yet, so legacy files are
///     forward-compatible; we just want the stamp going forward.
///   * `version > FOLDERS_INDEX_VERSION` — a newer CLI wrote this
///     file. Refuse to interpret it and warn; the next write of the
///     index by this older CLI would silently downgrade it, so we
///     return a fresh empty index and let `knack pull` repopulate.
pub fn read_folders_index(workspace: &Path) -> io::Result<FoldersIndex> {
    let path = workspace.join(FOLDERS_INDEX_FILE);
    match fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<FoldersIndex>(&s) {
            Ok(idx) if idx.version == FOLDERS_INDEX_VERSION => Ok(idx),
            Ok(mut idx) if idx.version == 0 => {
                // Pre-versioning file. Same shape; just adopt it.
                idx.version = FOLDERS_INDEX_VERSION;
                Ok(idx)
            }
            Ok(idx) => {
                eprintln!(
                    "knack: ignoring {path:?} — schema version {} not understood (current: {}); will rebuild on next pull",
                    idx.version, FOLDERS_INDEX_VERSION
                );
                Ok(FoldersIndex::current())
            }
            Err(e) => {
                eprintln!(
                    "knack: ignoring {path:?} — malformed JSON ({e}); will rebuild on next pull"
                );
                Ok(FoldersIndex::current())
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FoldersIndex::current()),
        Err(e) => Err(e),
    }
}

/// Persist the folder index atomically (tempfile + rename in the same
/// dir, mirroring ``commands/install/installed.rs`` so a crash mid-write
/// never leaves a half-written JSON blob). Stamps the current schema
/// version on the way out so future CLIs can read it back via the
/// version-aware loader.
pub fn write_folders_index(workspace: &Path, idx: &FoldersIndex) -> io::Result<()> {
    fs::create_dir_all(workspace)?;
    let path = workspace.join(FOLDERS_INDEX_FILE);
    let mut out = idx.clone();
    out.version = FOLDERS_INDEX_VERSION;
    let body = serde_json::to_string_pretty(&out)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Stamp a (folder_name, slug) pair into the index. Creates the folder
/// entry if missing. Removes the slug from any other folder it was in
/// (a skill belongs to at most one folder). No-op when ``folder_name``
/// is None — caller should call ``remove_from_folder`` instead. Returns
/// whether the index actually changed.
pub fn assign_to_folder(
    idx: &mut FoldersIndex,
    slug: &str,
    folder_id: &str,
    folder_name: &str,
    scope: &str,
    owner_team_id: Option<&str>,
) -> bool {
    let mut changed = false;
    // Strip this slug from any folder it was previously assigned to.
    for entry in &mut idx.folders {
        if entry.id == folder_id {
            continue;
        }
        let before = entry.slugs.len();
        entry.slugs.retain(|s| s != slug);
        if entry.slugs.len() != before {
            changed = true;
        }
    }

    if let Some(entry) = idx.folders.iter_mut().find(|e| e.id == folder_id) {
        // Update name/scope in case the server renamed since we last
        // wrote — cheap reconciliation per pull.
        if entry.name != folder_name {
            entry.name = folder_name.to_string();
            changed = true;
        }
        if entry.scope != scope {
            entry.scope = scope.to_string();
            changed = true;
        }
        if entry.owner_team_id.as_deref() != owner_team_id {
            entry.owner_team_id = owner_team_id.map(str::to_string);
            changed = true;
        }
        if !entry.slugs.iter().any(|s| s == slug) {
            entry.slugs.push(slug.to_string());
            changed = true;
        }
    } else {
        idx.folders.push(FolderIndexEntry {
            id: folder_id.to_string(),
            name: folder_name.to_string(),
            scope: scope.to_string(),
            owner_team_id: owner_team_id.map(str::to_string),
            slugs: vec![slug.to_string()],
        });
        changed = true;
    }

    // Stable order — folders sorted by name so `knack folder list` is
    // predictable.
    idx.folders.sort_by(|a, b| a.name.cmp(&b.name));
    changed
}

/// Drop ``slug`` from every folder entry. Returns whether anything
/// changed. Folder entries that become empty are kept around — they
/// still exist server-side until `knack folder delete` removes them.
pub fn remove_from_folder(idx: &mut FoldersIndex, slug: &str) -> bool {
    let mut changed = false;
    for entry in &mut idx.folders {
        let before = entry.slugs.len();
        entry.slugs.retain(|s| s != slug);
        if entry.slugs.len() != before {
            changed = true;
        }
    }
    changed
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
        // The start path must live UNDER the fake HOME for the HOME
        // boundary to be the walk's actual stop point. Without that,
        // on a real machine the walk escapes the fake tempdir and
        // eventually reaches the user's real HOME — where `~/.knack/`
        // often exists and would match.
        let fake_home = tempdir().unwrap();
        let nested = fake_home.path().join("project").join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            discover_workspace_root_with_home(&nested, Some(fake_home.path())).is_none()
        );
    }

    #[test]
    fn discover_stops_at_home_even_when_home_knack_exists() {
        // The user's `~/.knack/` is the global pool, not a workspace
        // root every project under HOME inherits silently. A walk from
        // a subdir under HOME must stop at HOME and return None even
        // though `<home>/.knack/` exists.
        let fake_home = tempdir().unwrap();
        // Plant a `~/.knack/` to prove the walk skips it.
        fs::create_dir_all(fake_home.path().join(WORKSPACE_DIR_NAME)).unwrap();
        let project = fake_home.path().join("projects").join("x");
        fs::create_dir_all(&project).unwrap();
        assert!(
            discover_workspace_root_with_home(&project, Some(fake_home.path())).is_none(),
            "discover_workspace_root_with_home must not climb into HOME"
        );
    }

    #[test]
    fn discover_finds_workspace_above_start_below_home() {
        // The HOME stop must NOT prevent finding a real workspace marker
        // somewhere in between the start path and HOME. Plant
        // `<home>/projects/repo/.knack/`, start the walk from
        // `<home>/projects/repo/src/`, and expect a hit.
        let fake_home = tempdir().unwrap();
        let repo = fake_home.path().join("projects").join("repo");
        fs::create_dir_all(repo.join(WORKSPACE_DIR_NAME)).unwrap();
        let src = repo.join("src");
        fs::create_dir_all(&src).unwrap();
        let found =
            discover_workspace_root_with_home(&src, Some(fake_home.path())).unwrap();
        let expected = repo.canonicalize().unwrap().join(WORKSPACE_DIR_NAME);
        assert_eq!(found, expected);
    }

    #[test]
    fn resolve_skills_root_prefers_explicit_target() {
        let cwd = tempdir().unwrap();
        let target = PathBuf::from("/tmp/custom-target");
        let home = PathBuf::from("/home/jane/.knack/skills");
        let resolved = resolve_skills_root(cwd.path(), false, Some(&target), &home);
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
        assert_eq!(
            resolved,
            canonical_root.join(WORKSPACE_DIR_NAME).join(SKILLS_SUBDIR)
        );
    }

    #[test]
    fn resolve_skills_root_falls_back_to_cwd_dot_knack() {
        // `resolve_skills_root` internally calls `discover_workspace_root`
        // which now stops at the user's real HOME. The tempdir lives
        // under HOME on every platform we run on, so the walk halts at
        // HOME (before finding any `~/.knack/`) and falls back to
        // `<cwd>/.knack/skills/`. The previous test design relied on
        // the walk hitting filesystem root; that was the bug.
        let root = tempdir().unwrap();
        let home = PathBuf::from("/home/jane/.knack/skills");

        let resolved = resolve_skills_root(root.path(), false, None, &home);
        assert_eq!(
            resolved,
            root.path().join(WORKSPACE_DIR_NAME).join(SKILLS_SUBDIR),
        );
        let _ = env::current_dir();
    }

    #[test]
    fn init_workspace_adds_dot_knack_to_root_gitignore_inside_git_repo() {
        // The publish path's `has_unrelated_dirty` check refuses any
        // uncommitted state outside `skills/<slug>/`. After a fresh
        // `knack init --self-host --skip-bootstrap` over a cloned
        // repo, the `.knack/` workspace dir lands as untracked state
        // and breaks the next publish. init_workspace now writes a
        // `.knack/` line into the root `.gitignore` so the workspace
        // is never visible to `git status` in the first place.
        let repo = tempdir().unwrap();
        // Pretend it's a git repo by planting `.git/`.
        fs::create_dir_all(repo.path().join(".git")).unwrap();

        init_workspace(repo.path()).unwrap();

        let gitignore = repo.path().join(".gitignore");
        assert!(gitignore.is_file(), "root .gitignore must be created");
        let body = fs::read_to_string(&gitignore).unwrap();
        assert!(
            body.lines().any(|l| l.trim() == ".knack/"),
            "root .gitignore should contain a `.knack/` line, got: {body:?}"
        );
    }

    #[test]
    fn init_workspace_preserves_existing_root_gitignore() {
        // Don't trample a hand-edited root .gitignore — just append.
        let repo = tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".git")).unwrap();
        fs::write(
            repo.path().join(".gitignore"),
            "node_modules/\ndist/\n",
        )
        .unwrap();

        init_workspace(repo.path()).unwrap();

        let body = fs::read_to_string(repo.path().join(".gitignore")).unwrap();
        assert!(body.contains("node_modules/"), "existing entry must survive");
        assert!(body.contains("dist/"), "existing entry must survive");
        assert!(body.lines().any(|l| l.trim() == ".knack/"));
    }

    #[test]
    fn init_workspace_skips_root_gitignore_when_not_a_git_repo() {
        // Outside a git repo there's no value polluting the user's
        // directory with a stray `.gitignore`. Only do it when the
        // gate fires (i.e. when `.git/` is present).
        let dir = tempdir().unwrap();
        init_workspace(dir.path()).unwrap();
        assert!(!dir.path().join(".gitignore").exists(),
            "non-git contexts shouldn't get a generated root .gitignore");
    }

    #[test]
    fn init_workspace_root_gitignore_is_idempotent() {
        // Re-running `knack init` in the same repo shouldn't keep
        // appending `.knack/` lines.
        let repo = tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".git")).unwrap();
        init_workspace(repo.path()).unwrap();
        init_workspace(repo.path()).unwrap();
        init_workspace(repo.path()).unwrap();
        let body = fs::read_to_string(repo.path().join(".gitignore")).unwrap();
        let count = body.lines().filter(|l| l.trim() == ".knack/").count();
        assert_eq!(count, 1, "exactly one `.knack/` line expected, got {count}: {body:?}");
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
        let resolved = resolve_existing_skill_dir("foo", root.path(), &home).unwrap();
        // `discover_workspace_root` canonicalizes on Unix (no-op) and
        // emits a `\\?\` UNC-prefixed path on Windows. Compare against
        // the same canonical form so the test passes everywhere.
        let canonical_drafts = drafts.canonicalize().unwrap();
        assert_eq!(resolved.canonicalize().unwrap(), canonical_drafts);
    }
}
