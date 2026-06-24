//! Chunking for the agent playbook (`agent.txt`, surfaced by `knack info`).
//!
//! The full playbook is ~19k tokens; an agent almost always needs only one
//! section (run a skill, publish, conduct the interview). This module slices
//! the *single* playbook file — whether freshly fetched or the bundled
//! fallback — into addressable chunks keyed by stable slugs, so the meta-skill
//! can route a task to the exact section(s) to pull. We slice one source of
//! truth rather than fork content, so web and CLI never drift.
//!
//! Boundaries are the playbook's column-0 `=== … ===` marker lines. Markers
//! that are indented (e.g. the sub-sections inside PART SIX) are deliberately
//! NOT boundaries, so a section stays whole. The leading text before the first
//! marker is the `overview` chunk.

/// One addressable section of the playbook.
pub struct Chunk {
    /// Stable slug — used on the CLI (`knack info <slug>`) and hardcoded in the
    /// meta-skill router. Treat renames as breaking.
    pub slug: &'static str,
    /// Case-insensitive substring identifying this section's `=== … ===`
    /// marker line. The empty string is special-cased to the intro (everything
    /// before the first marker).
    pub marker: &'static str,
    /// One-line table-of-contents blurb.
    pub blurb: &'static str,
}

/// Ordered chunk table — the single source of stable slugs + TOC text. Kept in
/// lockstep with the `=== … ===` markers in `embedded/agent.txt`; the
/// `every_slug_resolves` test fails CI if a marker is renamed out from under a
/// slug.
pub const CHUNKS: &[Chunk] = &[
    Chunk {
        slug: "overview",
        marker: "",
        blurb: "What knack is, install-step framing, how to read the playbook.",
    },
    Chunk {
        slug: "persistence",
        marker: "ABOUT PERSISTENCE",
        blurb: "How the meta-skill persists; ephemeral vs durable environments.",
    },
    Chunk {
        slug: "setup",
        marker: "PART ONE: SETUP",
        blurb: "Install the CLI: paths, sandboxes, no-sudo (PART ONE).",
    },
    Chunk {
        slug: "brief",
        marker: "BRIEF THE USER",
        blurb: "First-time framing to give the user before starting.",
    },
    Chunk {
        slug: "interview",
        marker: "CONDUCTING THE INTERVIEW",
        blurb: "The interview: Genesis, Artifacts, Intuition, Dry Run (PART TWO).",
    },
    Chunk {
        slug: "authoring",
        marker: "PART THREE: AUTHORING",
        blurb: "Turn the interview into a SKILL.md + assets (PART THREE).",
    },
    Chunk {
        slug: "publishing",
        marker: "PART FOUR: PUBLISHING",
        blurb: "Publish, version, validate (PART FOUR).",
    },
    Chunk {
        slug: "iterating",
        marker: "PART FIVE: ITERATING",
        blurb: "Revise and re-publish; analyze runs (PART FIVE).",
    },
    Chunk {
        slug: "running",
        marker: "RUNNING THE SKILL",
        blurb: "pull -> run -> mark; inputs; run analytics (PART SIX).",
    },
    Chunk {
        slug: "sharing",
        marker: "SHARING + DISCOVERY",
        blurb: "Marketplace, usernames, forking, teams (PART SEVEN).",
    },
    Chunk {
        slug: "rules",
        marker: "=== RULES ===",
        blurb: "Hard constraints the agent must follow.",
    },
    Chunk {
        slug: "updating",
        marker: "=== UPDATING ===",
        blurb: "Upgrade the CLI.",
    },
    Chunk {
        slug: "uninstall",
        marker: "=== UNINSTALL ===",
        blurb: "Remove knack cleanly.",
    },
];

/// All valid slugs, for error messages.
pub fn slugs() -> Vec<&'static str> {
    CHUNKS.iter().map(|c| c.slug).collect()
}

/// Split `text` into sections at column-0 `===` lines. Each returned tuple is
/// `(marker_line, section_text)` where `section_text` includes the marker
/// line. The first element is always the intro: `marker_line == ""` covering
/// everything before the first marker (possibly empty).
fn split_sections(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur_marker = String::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        // Column-0 markers only: indented `===` (PART SIX sub-sections) stay
        // inside their parent chunk.
        if trimmed.starts_with("===") {
            out.push((cur_marker.clone(), std::mem::take(&mut cur)));
            cur_marker = trimmed.to_string();
        }
        cur.push_str(line);
    }
    out.push((cur_marker, cur));
    out
}

/// Return the section body (marker line included) for `slug`, or `None` if the
/// slug is unknown or its marker isn't found in `text`.
pub fn chunk(text: &str, slug: &str) -> Option<String> {
    let c = CHUNKS.iter().find(|c| c.slug.eq_ignore_ascii_case(slug))?;
    let sections = split_sections(text);
    if c.marker.is_empty() {
        // overview = the intro section (the one with an empty marker).
        return sections
            .into_iter()
            .next()
            .filter(|(m, b)| m.is_empty() && !b.trim().is_empty())
            .map(|(_, b)| b);
    }
    let needle = c.marker.to_ascii_uppercase();
    sections
        .into_iter()
        .find(|(m, _)| m.to_ascii_uppercase().contains(&needle))
        .map(|(_, b)| b)
}

/// Render the table of contents from [`CHUNKS`].
pub fn toc() -> String {
    let mut s = String::from(
        "knack playbook sections — pull one (or several) with `knack info <slug> [<slug>...]`:\n\n",
    );
    for c in CHUNKS {
        s.push_str(&format!("  {:<12} {}\n", c.slug, c.blurb));
    }
    s.push_str("\n`knack info` with no slug prints the entire playbook.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLED: &str = include_str!("../../embedded/agent.txt");

    #[test]
    fn every_slug_resolves_to_a_nonempty_section() {
        for c in CHUNKS {
            let got = chunk(BUNDLED, c.slug)
                .unwrap_or_else(|| panic!("slug `{}` did not resolve", c.slug));
            assert!(
                !got.trim().is_empty(),
                "slug `{}` resolved to an empty section",
                c.slug
            );
        }
    }

    #[test]
    fn overview_is_the_intro_only() {
        let ov = chunk(BUNDLED, "overview").unwrap();
        assert!(
            ov.contains("This is documentation for the Knack CLI"),
            "overview should contain the intro preamble"
        );
        assert!(
            !ov.contains("=== ABOUT PERSISTENCE"),
            "overview must stop before the first === marker"
        );
    }

    #[test]
    fn running_keeps_nested_markers_and_does_not_bleed_into_sharing() {
        let running = chunk(BUNDLED, "running").unwrap();
        assert!(running.contains("RUNNING THE SKILL"));
        // Indented sub-markers inside PART SIX must remain part of `running`.
        assert!(
            running.contains("ORIENTATION: WHERE DO I START"),
            "nested indented === markers should stay inside the running chunk"
        );
        // And it must not run past its boundary into PART SEVEN.
        assert!(
            !running.contains("SHARING + DISCOVERY"),
            "running chunk should not bleed into the sharing section"
        );
    }

    #[test]
    fn distinct_sections_do_not_collide() {
        let interview = chunk(BUNDLED, "interview").unwrap();
        let authoring = chunk(BUNDLED, "authoring").unwrap();
        assert!(interview.contains("CONDUCTING THE INTERVIEW"));
        assert!(authoring.contains("AUTHORING"));
        assert_ne!(interview, authoring);
    }

    #[test]
    fn unknown_slug_is_none() {
        assert!(chunk(BUNDLED, "bogus").is_none());
    }

    #[test]
    fn toc_lists_every_slug() {
        let t = toc();
        for c in CHUNKS {
            assert!(t.contains(c.slug), "TOC missing slug {}", c.slug);
        }
    }
}
