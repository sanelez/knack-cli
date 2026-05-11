//! Detect which coding agent is currently driving knack.
//!
//! Two signals, in order of strength:
//!   1. **Env markers** — the agent is actively running in this process
//!      tree (e.g. `CLAUDECODE=1` set by Claude Code on every child).
//!   2. **Binary markers** — the agent is installed but might not be the
//!      one currently shelling out. Weaker signal, used only when no env
//!      marker hit.
//!
//! Returns at most one detected target (first match). `generic` is excluded
//! from autodetect — `mod.rs::run` adds it separately as a safety net so
//! every install lays down at least one AGENTS.md somewhere.

use super::targets::{AgentTarget, TARGETS};

pub fn autodetect() -> Option<&'static AgentTarget> {
    for t in TARGETS {
        if t.name == "generic" {
            continue;
        }
        if t.env_markers.iter().any(|k| std::env::var(k).is_ok()) {
            return Some(t);
        }
    }
    for t in TARGETS {
        if t.name == "generic" {
            continue;
        }
        if t.binary_markers.iter().any(|b| binary_on_path(b)) {
            return Some(t);
        }
    }
    None
}

fn binary_on_path(name: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    for dir in std::env::split_paths(&path) {
        if dir.join(name).is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            if dir.join(format!("{name}.exe")).is_file() {
                return true;
            }
        }
    }
    false
}
