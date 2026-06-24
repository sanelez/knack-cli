//! `knack info` — print the canonical Knack agent playbook (agent.txt).
//!
//! Fetches `https://getknack.ai/agent.txt` so the output is always current.
//! On any network failure, falls back to a copy bundled into the binary at
//! build time via `include_str!`. The bundled copy lives next to the web
//! origin (`apps/web/public/agent.txt`) so the source of truth is a single
//! file — release builds embed whatever the web is serving at tag time.
//!
//! The playbook is ~19k tokens. To keep agent context lean, callers can pull
//! one or more named sections instead of the whole thing:
//!   * `knack info`                  — the entire playbook (compaction recovery)
//!   * `knack info --list`           — the section index (offline; no fetch)
//!   * `knack info running`          — just PART SIX
//!   * `knack info interview authoring` — several sections, concatenated
//!
//! Sections are sliced from the same single source file (see [`info_chunks`]),
//! so there is no separate content to maintain.
//!
//! Output is plain stdout, no decoration: the consumer is an LLM, not a
//! human, and ANSI/structured formatting just adds noise to the prompt.

use clap::Args;

use crate::commands::info_chunks;
use crate::errors::CliResult;
use crate::output::OutputMode;

/// Embedded fallback agent.txt — synced from the marketing site at release time.
const BUNDLED: &str = include_str!("../../embedded/agent.txt");

const REMOTE_URL: &str = "https://getknack.ai/agent.txt";

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Playbook section(s) to print (e.g. `running`, `interview authoring`).
    /// Omit to print the whole playbook. See `knack info --list`.
    pub topics: Vec<String>,

    /// Print the section index (slugs + one-line blurbs) and exit. Offline.
    #[arg(long)]
    pub list: bool,

    /// Skip the network fetch and print the bundled copy directly.
    #[arg(long)]
    pub offline: bool,
}

pub async fn run(args: InfoArgs, mode: OutputMode) -> CliResult<()> {
    // --list is a pure local operation; never touches the network.
    if args.list {
        let toc = info_chunks::toc();
        if mode.json {
            let payload = serde_json::json!({
                "sections": info_chunks::CHUNKS.iter().map(|c| serde_json::json!({
                    "slug": c.slug, "blurb": c.blurb,
                })).collect::<Vec<_>>(),
            });
            println!("{payload}");
        } else {
            print!("{toc}");
        }
        return Ok(());
    }

    let (full, source) = if args.offline {
        (BUNDLED.to_string(), "bundled")
    } else {
        match fetch_remote().await {
            Ok(b) => (b, "remote"),
            Err(_) => (BUNDLED.to_string(), "bundled"),
        }
    };

    // No topics → the whole playbook (back-compat + compaction recovery).
    if args.topics.is_empty() {
        return emit(mode, &full, source, &[], args.offline);
    }

    // One or more topics → slice the requested sections. Unknown slugs are
    // warned about (stderr, so stdout stays a clean playbook for the agent)
    // but don't abort as long as at least one resolved.
    let mut pieces: Vec<String> = Vec::new();
    let mut resolved: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for t in &args.topics {
        match info_chunks::chunk(&full, t) {
            Some(body) => {
                resolved.push(t.clone());
                pieces.push(body.trim_end().to_string());
            }
            None => unknown.push(t.clone()),
        }
    }

    if resolved.is_empty() {
        let err = crate::errors::CliError::User {
            code: "UNKNOWN_INFO_TOPIC".into(),
            message: format!("no known playbook section in {:?}", args.topics),
            hint: Some(format!("valid sections: {}", info_chunks::slugs().join(", "))),
        };
        crate::output::emit_err(mode, &err);
        return Err(err);
    }
    if !unknown.is_empty() && !mode.quiet {
        eprintln!(
            "knack info: ignoring unknown section(s) {:?} — valid: {}",
            unknown,
            info_chunks::slugs().join(", ")
        );
    }

    let header = format!(
        "(slice of the knack playbook: {}. Run `knack info` for the whole thing, \
`knack info --list` for the index.)\n\n",
        resolved.join(", ")
    );
    let body = format!("{header}{}", pieces.join("\n\n"));
    emit(mode, &body, source, &resolved, args.offline)
}

/// Print the resolved body, mirroring the prior stdout/JSON contract.
fn emit(
    mode: OutputMode,
    body: &str,
    source: &str,
    topics: &[String],
    offline: bool,
) -> CliResult<()> {
    if mode.json {
        let payload = serde_json::json!({
            "source": source,
            "url": REMOTE_URL,
            "topics": topics,
            "body": body,
        });
        println!("{payload}");
    } else {
        if source == "bundled" && !offline && !mode.quiet {
            // One-line warning to stderr so it doesn't taint stdout (agents
            // parse stdout as the playbook).
            eprintln!("knack info: using bundled copy (network fetch failed); try again online for the latest.");
        }
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

async fn fetch_remote() -> Result<String, reqwest::Error> {
    let client = crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()?;
    let resp = client.get(REMOTE_URL).send().await?.error_for_status()?;
    resp.text().await
}
