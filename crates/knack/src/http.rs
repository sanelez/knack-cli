//! Single place every HTTPS client in the CLI is built, so the TLS trust
//! policy is applied uniformly.
//!
//! reqwest is compiled with the OS trust store + bundled webpki roots (see
//! the workspace Cargo.toml), which already covers most corporate
//! TLS-inspecting proxies — their CA lives in the OS keychain. On top of
//! that, [`crate::http`] honors a custom CA bundle and an insecure escape
//! hatch resolved by [`knack_types::tls`] from `--cacert` / `--insecure`
//! and the `KNACK_CA_BUNDLE` / `SSL_CERT_FILE` / `KNACK_INSECURE`
//! environment variables.

pub const USER_AGENT: &str = concat!("knack-cli/", env!("CARGO_PKG_VERSION"));

/// A `reqwest::ClientBuilder` with Knack's user-agent and TLS policy
/// applied. Callers may layer on timeouts etc. before `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    apply_tls(reqwest::Client::builder().user_agent(USER_AGENT))
}

/// A ready client for one-shot requests (bundle up/download to presigned
/// URLs, etc.). Falls back to a bare client only if the builder fails,
/// which in practice it won't.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Where a connectivity probe stopped. Drives the operator-facing hint in
/// `knack debug --connectivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStage {
    /// Reached the host and got an HTTP response (any status, even 4xx).
    Ok,
    /// TLS handshake failed — almost always a TLS-inspecting proxy whose
    /// CA isn't trusted. Points the user straight at `--cacert`.
    Tls,
    /// DNS / TCP connect failed (host blocked, firewalled, or no route).
    Connect,
    /// Connected but the request didn't complete in time.
    Timeout,
}

impl ProbeStage {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStage::Ok => "OK",
            ProbeStage::Tls => "TLS",
            ProbeStage::Connect => "CONNECT",
            ProbeStage::Timeout => "TIMEOUT",
        }
    }
}

/// Result of probing one host.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub stage: ProbeStage,
    /// HTTP status code when the host answered, else None.
    pub http_status: Option<u16>,
    /// Short human detail (error summary or status line).
    pub detail: String,
}

fn looks_like_tls_error(err: &reqwest::Error) -> bool {
    // reqwest collapses rustls handshake failures into a connect error;
    // the cert/verify wording only lives in the source chain, so walk it.
    let mut src: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = src {
        let s = e.to_string().to_ascii_lowercase();
        if s.contains("certificate")
            || s.contains("cert ")
            || s.contains("tls")
            || s.contains("handshake")
            || s.contains("verify")
            || s.contains("self-signed")
            || s.contains("unknownissuer")
            || s.contains("invalidcertificate")
        {
            return true;
        }
        src = e.source();
    }
    false
}

/// Probe one host over HTTPS with a short timeout, classifying the outcome
/// so a corporate-network failure reads as "host X blocked at stage Y"
/// instead of a generic NETWORK error. Any HTTP status counts as reachable
/// — a 401/403/404 still proves DNS + TCP + TLS all worked. Honors the
/// process TLS policy (so a passing TLS stage also proves a `--cacert` /
/// native-roots fix is working).
pub async fn probe_host(host: &str) -> ProbeResult {
    let url = format!("https://{host}/");
    let http = match client_builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult {
                stage: ProbeStage::Connect,
                http_status: None,
                detail: format!("client build failed: {e}"),
            }
        }
    };
    match http.get(&url).send().await {
        Ok(resp) => ProbeResult {
            stage: ProbeStage::Ok,
            http_status: Some(resp.status().as_u16()),
            detail: format!("HTTP {}", resp.status().as_u16()),
        },
        Err(e) => {
            let stage = if e.is_timeout() {
                ProbeStage::Timeout
            } else if looks_like_tls_error(&e) {
                ProbeStage::Tls
            } else {
                ProbeStage::Connect
            };
            ProbeResult {
                stage,
                http_status: None,
                detail: e.to_string(),
            }
        }
    }
}

/// Apply the process TLS settings to any builder. Shared so a builder that
/// needs custom timeouts still gets the custom-CA / insecure handling.
pub fn apply_tls(mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let tls = knack_types::tls::settings();
    if let Some(path) = tls.ca_bundle.as_deref() {
        match std::fs::read(path) {
            Ok(pem) => match reqwest::Certificate::from_pem_bundle(&pem) {
                Ok(certs) if certs.is_empty() => eprintln!(
                    "knack: warning: no PEM certificates found in CA bundle {}",
                    path.display()
                ),
                Ok(certs) => {
                    for cert in certs {
                        builder = builder.add_root_certificate(cert);
                    }
                }
                Err(e) => eprintln!(
                    "knack: warning: ignoring CA bundle {} (parse failed: {e})",
                    path.display()
                ),
            },
            Err(e) => eprintln!(
                "knack: warning: ignoring CA bundle {} (read failed: {e})",
                path.display()
            ),
        }
    }
    if tls.insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder
}
