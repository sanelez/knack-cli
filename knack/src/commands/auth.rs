//! `knack auth` — login (device flow), logout, status.

use std::time::Duration;

use clap::Subcommand;
use serde_json::json;
use tokio::time::sleep;

use crate::api::ApiClient;
use crate::api::auth as api_auth;
use crate::auth_store::StoredTokens;
use crate::errors::{CliError, CliResult};
use crate::output::{OutputMode, chatter, emit_err, emit_ok};

#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Sign in via the browser (OAuth 2.0 device flow)
    Login(LoginArgs),
    /// Revoke the current session and forget the local tokens
    Logout,
    /// Print the current authenticated user
    Status,
}

#[derive(Debug, clap::Args)]
pub struct LoginArgs {
    /// Don't try to open the browser; just print the URL.
    #[arg(long)]
    pub no_browser: bool,
}

pub async fn run(cmd: AuthCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match cmd {
        AuthCmd::Login(a) => login(a, client, mode).await,
        AuthCmd::Logout => logout(client, mode).await,
        AuthCmd::Status => status(client, mode).await,
    }
}

async fn login(args: LoginArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let start = match api_auth::device_start(&client).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    chatter(mode, format!("Opening {} ...", start.verification_uri));
    chatter(
        mode,
        format!(
            "If a browser doesn't open, visit it manually. Code: {}",
            start.user_code
        ),
    );

    if !args.no_browser {
        // webbrowser::open is synchronous and best-effort; failure isn't fatal.
        let _ = webbrowser::open(&start.verification_uri);
    } else {
        // In --no-browser mode the URL needs to be visible even if we're in
        // --quiet, so write it to stderr unconditionally.
        eprintln!("{}", start.verification_uri);
    }

    chatter(mode, "Waiting for browser approval...");

    let interval = Duration::from_secs(start.interval.max(1));
    let deadline = std::time::Instant::now()
        + Duration::from_secs(start.expires_in.max(60).min(3600) as u64);

    loop {
        sleep(interval).await;
        if std::time::Instant::now() >= deadline {
            let err = CliError::AuthFailed("device code expired".into());
            emit_err(mode, &err);
            return Err(err);
        }

        let resp = match api_auth::device_poll(&client, &start.device_code).await {
            Ok(r) => r,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        };

        use api_auth::PollStatus;
        match resp.status {
            PollStatus::AuthorizationPending => continue,
            PollStatus::SlowDown => {
                sleep(interval).await;
                continue;
            }
            PollStatus::Denied => {
                let err = CliError::AuthFailed("approval denied".into());
                emit_err(mode, &err);
                return Err(err);
            }
            PollStatus::Expired => {
                let err = CliError::AuthFailed("device code expired".into());
                emit_err(mode, &err);
                return Err(err);
            }
            PollStatus::Approved => {
                let access = resp.access_token.unwrap_or_default();
                let refresh = resp.refresh_token.unwrap_or_default();
                let expires = resp.expires_in.unwrap_or(0);
                if access.is_empty() || refresh.is_empty() {
                    let err = CliError::AuthFailed("server omitted token pair".into());
                    emit_err(mode, &err);
                    return Err(err);
                }
                let tokens = StoredTokens {
                    access_token: access,
                    refresh_token: refresh,
                    expires_at: chrono::Utc::now().timestamp() + expires,
                };
                client.store.save(&client.account, &tokens)?;

                // Confirm with /auth/me so we can show the user who they are.
                match api_auth::me(&client).await {
                    Ok(me) => {
                        emit_ok(
                            mode,
                            json!({
                                "user_id": me.id,
                                "email": me.email,
                                "name": me.name,
                                "plan": me.plan,
                                "account": client.account,
                            }),
                            || {
                                println!("✓ logged in as {}", me.email);
                            },
                        );
                    }
                    Err(_) => {
                        emit_ok(
                            mode,
                            json!({ "account": client.account, "ok": true }),
                            || {
                                println!("✓ logged in");
                            },
                        );
                    }
                }
                return Ok(());
            }
        }
    }
}

async fn logout(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Best-effort: revoke server-side first, then wipe locally regardless.
    let stored = client.store.load(&client.account)?;
    let refresh = stored.as_ref().map(|t| t.refresh_token.as_str());
    let _ = api_auth::logout(&client, refresh).await;
    client.store.clear(&client.account)?;

    emit_ok(mode, json!({ "account": client.account, "logged_out": true }), || {
        println!("logged out.");
    });
    Ok(())
}

async fn status(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if client.store.load(&client.account)?.is_none() && client.bearer_override.is_none() {
        let err = CliError::AuthRequired;
        emit_err(mode, &err);
        return Err(err);
    }
    match api_auth::me(&client).await {
        Ok(me) => {
            emit_ok(
                mode,
                json!({
                    "user_id": me.id,
                    "email": me.email,
                    "plan": me.plan,
                    "account": client.account,
                    "auth_method": me.auth_method,
                }),
                || {
                    println!("{} ({})", me.email, me.plan);
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}
