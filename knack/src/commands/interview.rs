//! `knack interview --local` — run the Track B interview from the terminal.
//!
//! v0 is type-only. The console UX is intentionally plain: print the
//! conductor's question, prompt for an answer on a single line, repeat. As
//! the model streams its reply we render token-by-token. When the phase
//! reaches `compile`, we kick the compiler endpoint and stream its progress
//! bars. On done, we print the new skill version id.
//!
//! Voice mode (Deepgram Flux WebSocket → live transcript) is deferred — the
//! audio capture path is heavy and not blocking for v1 demos.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use console::style;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::api::artifacts as api_art;
use crate::api::interview as api_iv;
use crate::api::skills as api_skills;
use crate::api::sse::EventStream;
use crate::api::ApiClient;
use crate::errors::{CliError, CliResult};
use crate::output::{OutputMode, chatter, emit_err, emit_ok};

#[derive(Debug, Args)]
pub struct InterviewArgs {
    /// CLI-native intake. The only mode this command supports today; the flag
    /// exists for parity with the spec wording.
    #[arg(long, default_value_t = true)]
    pub local: bool,

    /// Optional opening line. If supplied we send it as the first user turn
    /// instead of waiting for a prompt.
    #[arg(long)]
    pub starter: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuestionPayload {
    text: String,
}

#[derive(Debug, Deserialize)]
struct PhaseChangePayload {
    phase: String,
}

#[derive(Debug, Deserialize)]
struct DonePayload {
    #[serde(default)]
    skill_version_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorPayload {
    code: String,
    message: String,
}

pub async fn run(args: InterviewArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if !args.local {
        return Err(CliError::User {
            code: "INTERVIEW_LOCAL_ONLY".into(),
            message: "knack interview only supports --local for now".into(),
            hint: Some("the web flow lives at https://getknack.ai".into()),
        });
    }

    chatter(mode, "starting interview...");
    let session = api_iv::create_session(
        &client,
        &api_iv::SessionCreate {
            mode: "cli".into(),
            starter_prompt: args.starter.clone(),
        },
    )
    .await?;
    chatter(mode, format!("session {} ({})", session.id, session.current_phase));

    print_banner(mode, &session.current_phase);

    let mut current_phase = session.current_phase.clone();
    let mut starter = args.starter.clone();
    // Scratch skill lazily created on first artifact upload. Set as
    // target_skill_id at compile time so the compile reuses the same shell.
    let mut scratch_skill_id: Option<String> = None;

    loop {
        // ── Prompt the user ────────────────────────────────────────────────
        let raw_user_text = match starter.take() {
            Some(s) if !s.trim().is_empty() => s,
            _ => match read_user_line(mode)? {
                Some(line) => line,
                None => {
                    chatter(mode, "(end of input — exiting)");
                    return Ok(());
                }
            },
        };

        let trimmed = raw_user_text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "/quit" | "/exit") {
            chatter(mode, "exiting.");
            return Ok(());
        }
        if trimmed == "/help" {
            print_inline_help(mode);
            continue;
        }

        // ── Slash-command: /upload <role> <path> ──────────────────────────
        // Uploads the file, attaches it to the session, and synthesizes a
        // short message we send to the conductor so the artifacts phase
        // advances.
        let user_text = if let Some(rest) = trimmed.strip_prefix("/upload") {
            match handle_upload(&client, &session.id, &mut scratch_skill_id, rest, mode).await {
                Ok(synthetic) => synthetic,
                Err(e) => {
                    emit_err(mode, &e);
                    continue; // let the user retry
                }
            }
        } else {
            raw_user_text
        };

        // ── Stream the conductor's reply ──────────────────────────────────
        let resp = api_iv::submit_answer_streaming(
            &client,
            &session.id,
            &api_iv::AnswerBody {
                text: user_text,
                is_voice: false,
            },
        )
        .await?;
        let mut stream = EventStream::from_response(resp);

        // Newline before the assistant's reply, mode-aware indent.
        if !mode.json {
            print!("\n{} ", indicator("knack:", mode));
            std::io::stdout().flush().ok();
        }

        let mut full_question: Option<String> = None;
        while let Some(ev) = stream.next().await? {
            match ev.name.as_str() {
                "transcript" => {
                    #[derive(Deserialize)]
                    struct D {
                        delta: String,
                    }
                    if let Ok(d) = ev.parse::<D>() {
                        if !mode.json {
                            print!("{}", d.delta);
                            std::io::stdout().flush().ok();
                        }
                    }
                }
                "question" => {
                    if let Ok(q) = ev.parse::<QuestionPayload>() {
                        full_question = Some(q.text);
                    }
                }
                "phase_change" => {
                    if let Ok(pc) = ev.parse::<PhaseChangePayload>() {
                        if pc.phase != current_phase {
                            current_phase = pc.phase.clone();
                            if !mode.json {
                                println!();
                                println!();
                                print_banner(mode, &current_phase);
                            }
                        }
                    }
                }
                "rule" => {
                    if !mode.json {
                        #[derive(Deserialize)]
                        struct R {
                            text: String,
                            kind: String,
                        }
                        if let Ok(r) = ev.parse::<R>() {
                            println!();
                            chatter(
                                mode,
                                format!("  · captured ({}) {}", r.kind, r.text),
                            );
                        }
                    }
                }
                "error" => {
                    if let Ok(e) = ev.parse::<ErrorPayload>() {
                        let err = CliError::Server {
                            status: 500,
                            code: e.code,
                            message: e.message,
                        };
                        emit_err(mode, &err);
                        return Err(err);
                    }
                }
                "done" => break,
                _ => {}
            }
        }

        if !mode.json {
            println!();
        }

        // ── Phase-aware loop control ──────────────────────────────────────
        if current_phase == "compile" {
            return drive_compile(&client, &session.id, scratch_skill_id.clone(), mode).await;
        }
        if current_phase == "publish" {
            chatter(mode, "interview complete.");
            return Ok(());
        }

        let _ = full_question; // we already streamed it; future TUI may use it
    }
}

async fn drive_compile(
    client: &ApiClient,
    session_id: &str,
    target_skill_id: Option<String>,
    mode: OutputMode,
) -> CliResult<()> {
    chatter(mode, "compiling...");
    let resp = api_iv::compile_streaming(
        client,
        session_id,
        &api_iv::CompileBody {
            target_skill_id,
            target_version: "0.1.0".into(),
        },
    )
    .await?;
    let mut stream = EventStream::from_response(resp);

    let mut last_progress: std::collections::HashMap<String, f64> = Default::default();
    let mut version_id: Option<String> = None;

    while let Some(ev) = stream.next().await? {
        match ev.name.as_str() {
            "compile_progress" => {
                #[derive(Deserialize)]
                struct CP {
                    file: String,
                    pct: f64,
                }
                if let Ok(p) = ev.parse::<CP>() {
                    let prev = last_progress.get(&p.file).copied().unwrap_or(0.0);
                    if (p.pct - prev) > 0.1 || (p.pct >= 1.0 && prev < 1.0) {
                        last_progress.insert(p.file.clone(), p.pct);
                        if !mode.json {
                            chatter(
                                mode,
                                format!("  · {:>14}  {:.0}%", p.file, p.pct * 100.0),
                            );
                        }
                    }
                }
            }
            "done" => {
                if let Ok(d) = ev.parse::<DonePayload>() {
                    version_id = d.skill_version_id;
                }
                break;
            }
            "error" => {
                if let Ok(e) = ev.parse::<ErrorPayload>() {
                    let err = CliError::Server {
                        status: 500,
                        code: e.code,
                        message: e.message,
                    };
                    emit_err(mode, &err);
                    return Err(err);
                }
            }
            _ => {}
        }
    }

    emit_ok(
        mode,
        json!({
            "session_id": session_id,
            "skill_version_id": version_id,
        }),
        || {
            if let Some(v) = &version_id {
                println!("✓ skill compiled · {v}");
            } else {
                println!("✓ compile finished");
            }
        },
    );
    Ok(())
}

fn read_user_line(mode: OutputMode) -> CliResult<Option<String>> {
    if !mode.json {
        print!("{} ", indicator("you:", mode));
        std::io::stdout().flush().ok();
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    let bytes = stdin.lock().read_line(&mut line)?;
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
}

fn indicator(label: &str, mode: OutputMode) -> String {
    if mode.no_color {
        label.to_string()
    } else {
        style(label).bold().cyan().to_string()
    }
}

/// Parse `/upload <role> <path>` (role optional — defaults to "input").
fn parse_upload_args(rest: &str) -> Result<(String, PathBuf), CliError> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let (role, path) = match parts.as_slice() {
        [path] => ("input", *path),
        [role, path] => (*role, *path),
        _ => {
            return Err(CliError::User {
                code: "UPLOAD_USAGE".into(),
                message: "usage: /upload [input|output|example] <path>".into(),
                hint: Some("e.g. /upload input ./Q1-receipts.xlsx".into()),
            });
        }
    };
    if !["input", "output", "example"].contains(&role) {
        return Err(CliError::User {
            code: "UPLOAD_BAD_ROLE".into(),
            message: format!("role must be input|output|example, got `{role}`"),
            hint: None,
        });
    }
    Ok((role.to_string(), PathBuf::from(path)))
}

/// Read + hash + upload + finalize + attach. Returns the synthetic message
/// the caller should feed back to the conductor as the user's "answer" for
/// the artifacts phase.
async fn handle_upload(
    client: &ApiClient,
    session_id: &str,
    scratch_skill_id: &mut Option<String>,
    rest: &str,
    mode: OutputMode,
) -> CliResult<String> {
    let (role, path) = parse_upload_args(rest.trim())?;

    // Read the file into memory. v0 cap at 100MB to keep behavior predictable;
    // for production we'd stream the body with a content-length header.
    let bytes = std::fs::read(&path).map_err(|e| CliError::User {
        code: "UPLOAD_READ_FAILED".into(),
        message: format!("can't read {}: {e}", path.display()),
        hint: Some("check the path is correct and readable".into()),
    })?;
    if bytes.len() > 100 * 1024 * 1024 {
        return Err(CliError::User {
            code: "UPLOAD_TOO_BIG".into(),
            message: format!("file is {} bytes; v0 cap is 100MB", bytes.len()),
            hint: Some("use the web flow for files > 100MB".into()),
        });
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload.bin")
        .to_string();

    // Lazily create a scratch skill so /artifacts/presign-upload has a parent.
    let skill_id = match scratch_skill_id.clone() {
        Some(id) => id,
        None => {
            let slug = scratch_slug(session_id);
            chatter(mode, format!("creating draft skill `{slug}`..."));
            let skill = api_skills::create(
                client,
                &api_skills::SkillCreate {
                    slug: slug.clone(),
                    name: "Interview draft".into(),
                    scope: Some("personal".into()),
                    owner_team_id: None,
                },
            )
            .await?;
            *scratch_skill_id = Some(skill.id.clone());
            skill.id
        }
    };

    chatter(mode, format!("uploading {} as {role}...", filename));
    let presigned = api_art::presign_upload(
        client,
        &api_art::PresignUploadRequest {
            skill_id: Some(skill_id),
            skill_version_id: None,
            kind: role.clone(),
            filename: filename.clone(),
            size_bytes: bytes.len() as u64,
        },
    )
    .await?;

    let body = bytes::Bytes::from(bytes.clone());
    api_art::put_bytes_to_presigned(
        client,
        &presigned.upload_url,
        body,
        guess_content_type(&path),
    )
    .await?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hex::encode(hasher.finalize());

    api_art::finalize(
        client,
        &presigned.artifact_id,
        &api_art::ArtifactFinalize {
            sha256,
            size_bytes: bytes.len() as u64,
        },
    )
    .await?;

    api_iv::attach_artifact(
        client,
        session_id,
        &api_iv::ArtifactAttachBody {
            artifact_id: presigned.artifact_id.clone(),
            role: role.clone(),
            filename: filename.clone(),
        },
    )
    .await?;

    Ok(format!(
        "I uploaded `{filename}` as the {role} ({} bytes).",
        bytes.len()
    ))
}

fn scratch_slug(session_id: &str) -> String {
    // Mirror the server's auto-generated slug so a later compile-without-id
    // path collides with itself rather than producing two skills.
    let head = session_id.replace('-', "");
    let head = &head[..head.len().min(8)];
    format!("interview-{head}")
}

fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("xlsx" | "xlsm") => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        }
        Some("csv") => "text/csv",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("txt" | "md") => "text/plain",
        _ => "application/octet-stream",
    }
}

fn print_inline_help(mode: OutputMode) {
    if mode.json {
        return;
    }
    let lines = [
        "  /upload [input|output|example] <path>   attach a file to the interview",
        "  /quit, /exit                            stop the interview",
        "  /help                                   show this list",
    ];
    for line in lines {
        eprintln!("{}", style(line).dim());
    }
}

fn print_banner(mode: OutputMode, phase: &str) {
    if mode.json {
        return;
    }
    let title = match phase {
        "genesis" => "── genesis ──",
        "artifacts" => "── artifacts ──",
        "intuition" => "── intuition ──",
        "compile" => "── compile ──",
        "refine" => "── refine ──",
        "publish" => "── publish ──",
        other => other,
    };
    println!("{}", style(title).dim());
}

#[cfg(test)]
mod tests {
    // Most of the interactive logic isn't testable without driving stdin and a
    // mock server. The SSE-event handling is exercised by api/sse.rs unit
    // tests; the command-shape itself is covered by the introspect tests.
    use super::*;

    #[test]
    fn smoke() {
        // Compiles is the assertion.
    }

    #[test]
    fn parse_upload_args_with_role_and_path() {
        let (role, path) = parse_upload_args("input ./Q1-receipts.xlsx").unwrap();
        assert_eq!(role, "input");
        assert_eq!(path, std::path::PathBuf::from("./Q1-receipts.xlsx"));
    }

    #[test]
    fn parse_upload_args_path_only_defaults_to_input() {
        let (role, path) = parse_upload_args("./foo.csv").unwrap();
        assert_eq!(role, "input");
        assert_eq!(path, std::path::PathBuf::from("./foo.csv"));
    }

    #[test]
    fn parse_upload_args_rejects_unknown_role() {
        let err = parse_upload_args("garbage ./foo").unwrap_err();
        assert_eq!(err.code(), "USER_ERROR");
    }

    #[test]
    fn parse_upload_args_rejects_empty() {
        assert!(parse_upload_args("").is_err());
    }

    #[test]
    fn parse_upload_args_rejects_too_many() {
        assert!(parse_upload_args("input ./a ./b").is_err());
    }

    #[test]
    fn scratch_slug_is_valid_per_server_regex() {
        let slug = scratch_slug("a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert!(slug.starts_with("interview-"));
        // Server pattern: ^[a-z0-9][a-z0-9-]*$ — verify our slug fits.
        let re = regex_lite("^[a-z0-9][a-z0-9-]*$");
        assert!(re(&slug), "{slug}");
    }

    fn regex_lite(pattern: &'static str) -> impl Fn(&str) -> bool {
        // Tiny inline matcher for the server's slug pattern; full regex
        // would be overkill for one test.
        let _ = pattern;
        |s: &str| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
                _ => return false,
            }
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
    }

    #[test]
    fn guess_content_type_basics() {
        assert_eq!(
            guess_content_type(std::path::Path::new("a.xlsx")),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(guess_content_type(std::path::Path::new("a.csv")), "text/csv");
        assert_eq!(guess_content_type(std::path::Path::new("a.pdf")), "application/pdf");
        assert_eq!(
            guess_content_type(std::path::Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
