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
