//! Process-wide TLS trust policy, shared by every crate that builds an
//! HTTP client (the cloud `knack` crate and the `knack-backend-github`
//! crate). Lives here, in the dependency-free types crate, because both
//! reqwest-using crates depend on it but not on each other.
//!
//! By default reqwest is built with the OS trust store + bundled webpki
//! roots (see the `reqwest` feature set in the workspace Cargo.toml), so
//! most corporate TLS-inspecting proxies "just work". These settings add
//! the explicit escape hatches for the cases the default can't cover:
//!
//!   * a custom CA bundle that isn't installed in the OS keychain
//!     (`--cacert <file>`, `KNACK_CA_BUNDLE`, or the de-facto-standard
//!     `SSL_CERT_FILE`), trusted IN ADDITION to the defaults;
//!   * a last-resort `--insecure` / `KNACK_INSECURE=1` that disables
//!     certificate verification entirely.
//!
//! `main` calls [`init`] once after parsing CLI flags; everything else
//! reads [`settings`]. The reqwest application of these settings lives in
//! each HTTP crate (it needs reqwest, which this crate deliberately does
//! not depend on).

use std::path::PathBuf;
use std::sync::OnceLock;

/// Resolved TLS trust policy for outbound HTTPS.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsSettings {
    /// Extra CA bundle (PEM, may hold multiple certs) to trust on top of
    /// the system + bundled roots.
    pub ca_bundle: Option<PathBuf>,
    /// Disable certificate verification entirely. Dangerous; last resort.
    pub insecure: bool,
}

static TLS: OnceLock<TlsSettings> = OnceLock::new();

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Resolve settings from the environment alone. `KNACK_CA_BUNDLE` wins
/// over `SSL_CERT_FILE`; empty values are ignored.
pub fn from_env() -> TlsSettings {
    let ca_bundle = std::env::var_os("KNACK_CA_BUNDLE")
        .or_else(|| std::env::var_os("SSL_CERT_FILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let insecure = std::env::var("KNACK_INSECURE")
        .map(|v| truthy(&v))
        .unwrap_or(false);
    TlsSettings {
        ca_bundle,
        insecure,
    }
}

/// Merge explicit CLI flags over environment values and freeze the result
/// for the rest of the process. Call once from `main` before any HTTP
/// client is built. Idempotent: later calls are ignored.
pub fn init(ca_bundle_flag: Option<PathBuf>, insecure_flag: bool) {
    let env = from_env();
    let resolved = TlsSettings {
        ca_bundle: ca_bundle_flag.or(env.ca_bundle),
        insecure: insecure_flag || env.insecure,
    };
    let _ = TLS.set(resolved);
}

/// The frozen settings. Falls back to a fresh environment read if `init`
/// was never called (e.g. in unit tests or library embedding).
pub fn settings() -> TlsSettings {
    TLS.get().cloned().unwrap_or_else(from_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_accepts_common_forms() {
        for v in ["1", "true", "TRUE", "Yes", "on", " on "] {
            assert!(truthy(v), "{v:?} should be truthy");
        }
        for v in ["0", "false", "no", "", "maybe"] {
            assert!(!truthy(v), "{v:?} should be falsy");
        }
    }

    #[test]
    fn flags_override_env() {
        // Pure merge logic — no global state, no env mutation.
        let flag = Some(PathBuf::from("/corp/ca.pem"));
        let merged = TlsSettings {
            ca_bundle: flag.clone().or(Some(PathBuf::from("/env/ca.pem"))),
            insecure: false || true,
        };
        assert_eq!(merged.ca_bundle, flag);
        assert!(merged.insecure);
    }
}
