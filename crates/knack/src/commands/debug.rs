//! `knack debug` — dump environment + config + recent state.
//!
//! For bug reports. Always emits JSON (the human format would mostly be the
//! same JSON pretty-printed). Tokens are never included in the output —
//! presence is reported as a boolean only.
//!
//! `--connectivity` turns the generic `NETWORK` error a corporate
//! TLS-inspecting proxy produces into a per-host "blocked at stage Y"
//! report, and doubles as proof the CA/native-roots fix works (a passing
//! TLS stage means the proxy cert is trusted).

use clap::Args;
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiClient;
use crate::errors::CliResult;
use crate::http::{probe_host, ProbeStage};
use crate::output::{emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DebugArgs {
    /// Probe every host the CLI needs to reach and report which one fails
    /// (and whether it's DNS/TCP, TLS, or a timeout). Works before login.
    #[arg(long)]
    pub connectivity: bool,
}

pub async fn run(args: DebugArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if args.connectivity {
        return connectivity(client, mode).await;
    }

    // We deliberately do not call /auth/me — the user might be debugging an
    // unreachable API; this command must work offline.
    let token_present = client.bearer_override.is_some()
        || client.store.load(&client.account).ok().flatten().is_some();

    let payload = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "config": {
            "api_base": client.config.api_base,
            "skills_dir": client.config.skills_dir,
            "keyring_service": client.config.keyring_service,
            "account": client.account,
        },
        "auth": {
            "token_present": token_present,
            "via_override": client.bearer_override.is_some(),
        },
        "env_relevant": {
            "KNACK_API_URL": std::env::var("KNACK_API_URL").ok(),
            "KNACK_SKILLS_DIR": std::env::var("KNACK_SKILLS_DIR").ok(),
            "KNACK_AUTH_TOKEN": std::env::var("KNACK_AUTH_TOKEN").ok().map(|_| "<set>".to_string()),
            "EDITOR": std::env::var("EDITOR").ok(),
            "VISUAL": std::env::var("VISUAL").ok(),
        },
    });

    emit_ok(mode, payload, || {
        println!("knack v{}", env!("CARGO_PKG_VERSION"));
        println!(
            "  os/arch     {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        println!("  api         {}", client.config.api_base);
        println!("  skills_dir  {}", client.config.skills_dir.display());
        println!("  account     {}", client.account);
        println!(
            "  token       {}",
            if token_present {
                "present"
            } else {
                "(not signed in)"
            }
        );
    });
    Ok(())
}

#[derive(Debug, Deserialize)]
struct HostEntry {
    host: String,
    #[serde(default)]
    purpose: String,
}

#[derive(Debug, Deserialize)]
struct ConnectivityHosts {
    hosts: Vec<HostEntry>,
}

/// Bare host[:port] from a URL or an already-bare host string.
fn host_of(url: &str) -> String {
    let no_scheme = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    no_scheme
        .split('/')
        .next()
        .unwrap_or(no_scheme)
        .trim_end_matches('/')
        .to_string()
}

async fn connectivity(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Authoritative list comes from the API (it knows its own storage
    // host); fall back to the hosts we can derive locally when the API
    // itself is unreachable — that's exactly the case worth diagnosing.
    let meta_url = format!("{}/meta/connectivity", client.config.api_base);
    let mut source = "api";
    let hosts: Vec<HostEntry> = match crate::http::client().get(&meta_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<ConnectivityHosts>().await {
            Ok(parsed) => parsed.hosts,
            Err(_) => {
                source = "fallback";
                fallback_hosts(&client)
            }
        },
        _ => {
            source = "fallback";
            fallback_hosts(&client)
        }
    };

    let mut results = Vec::with_capacity(hosts.len());
    for entry in &hosts {
        let probe = probe_host(&entry.host).await;
        results.push((entry, probe));
    }

    let any_tls = results.iter().any(|(_, p)| p.stage == ProbeStage::Tls);

    let payload = json!({
        "source": source,
        "connectivity": results
            .iter()
            .map(|(entry, p)| json!({
                "host": entry.host,
                "purpose": entry.purpose,
                "ok": p.stage == ProbeStage::Ok,
                "stage": p.stage.as_str(),
                "http_status": p.http_status,
                "detail": p.detail,
            }))
            .collect::<Vec<_>>(),
    });

    emit_ok(mode, payload, || {
        if source == "fallback" {
            println!("(couldn't fetch the host list from the API — probing locally");
            println!(" known hosts; the bundle-storage host may be missing below)");
        }
        println!();
        for (entry, p) in &results {
            let status = match p.http_status {
                Some(code) if p.stage == ProbeStage::Ok => format!("OK (HTTP {code})"),
                _ => p.stage.as_str().to_string(),
            };
            println!("  {:<13}  {:<46}  {}", status, entry.host, entry.purpose);
        }
        println!();
        if any_tls {
            println!("One or more TLS handshakes failed. That's the signature of a");
            println!("TLS-inspecting proxy whose CA the CLI doesn't trust. Point knack");
            println!("at the corporate CA:");
            println!();
            println!("    knack --cacert /path/to/corp-ca.pem auth login");
            println!("    # or: export KNACK_CA_BUNDLE=/path/to/corp-ca.pem");
            println!();
        }
    });
    Ok(())
}

/// Hosts derivable without the API: the configured API host plus the
/// public web + CLI-download hosts. The R2 storage host is intentionally
/// absent — only the API knows it, so we say so rather than guess.
fn fallback_hosts(client: &ApiClient) -> Vec<HostEntry> {
    vec![
        HostEntry {
            host: host_of(&client.config.api_base),
            purpose: "API".into(),
        },
        HostEntry {
            host: "getknack.ai".into(),
            purpose: "Web sign-in + docs".into(),
        },
        HostEntry {
            host: "cli.getknack.ai".into(),
            purpose: "CLI install/upgrade".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn host_of_strips_scheme_and_path() {
        assert_eq!(host_of("https://api.getknack.ai"), "api.getknack.ai");
        assert_eq!(host_of("https://api.getknack.ai/"), "api.getknack.ai");
        assert_eq!(host_of("http://localhost:8000/v1"), "localhost:8000");
        assert_eq!(host_of("api.getknack.ai"), "api.getknack.ai");
    }
}
