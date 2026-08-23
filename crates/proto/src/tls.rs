// SPDX-License-Identifier: Apache-2.0
//! Mutual TLS for `SnapshotService` (ADR-0021).
//!
//! Both sides read PEM material from one directory, matching the layout of
//! a Kubernetes TLS secret plus the issuing CA:
//!
//! - `tls.crt` / `tls.key` - this side's identity
//! - `ca.crt` - the CA that signed the *peer's* certificate
//!
//! The server requires client certificates signed by `ca.crt`; the client
//! verifies the server against `ca.crt` and presents its own identity.
//! Plain TCP remains the default when no directory is configured - snapshot
//! distribution inside one trusted cluster network predates this and keeps
//! working unchanged.

use std::path::Path;

use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

/// Failure to assemble a TLS configuration.
#[derive(Debug, thiserror::Error)]
pub enum TlsConfigError {
    /// A PEM file was missing or unreadable.
    #[error("reading {name} from {dir}: {source}")]
    Read {
        /// File name within the directory.
        name: &'static str,
        /// The configured directory.
        dir: String,
        /// Underlying error.
        source: std::io::Error,
    },
}

fn read(dir: &Path, name: &'static str) -> Result<Vec<u8>, TlsConfigError> {
    std::fs::read(dir.join(name)).map_err(|source| TlsConfigError::Read {
        name,
        dir: dir.display().to_string(),
        source,
    })
}

/// Server-side mTLS: present `tls.crt`/`tls.key`, require client
/// certificates signed by `ca.crt`.
///
/// # Errors
/// [`TlsConfigError`] when a PEM file cannot be read.
pub fn server_mtls(dir: &Path) -> Result<ServerTlsConfig, TlsConfigError> {
    let identity = Identity::from_pem(read(dir, "tls.crt")?, read(dir, "tls.key")?);
    let ca = Certificate::from_pem(read(dir, "ca.crt")?);
    Ok(ServerTlsConfig::new()
        .identity(identity)
        .client_ca_root(ca)
        .client_auth_optional(false))
}

/// Client-side mTLS: verify the server against `ca.crt`, present
/// `tls.crt`/`tls.key`. `domain` overrides the name checked against the
/// server certificate when it differs from the dial address (for example
/// dialing a port-forward).
///
/// # Errors
/// [`TlsConfigError`] when a PEM file cannot be read.
pub fn client_mtls(dir: &Path, domain: Option<&str>) -> Result<ClientTlsConfig, TlsConfigError> {
    let identity = Identity::from_pem(read(dir, "tls.crt")?, read(dir, "tls.key")?);
    let ca = Certificate::from_pem(read(dir, "ca.crt")?);
    let mut config = ClientTlsConfig::new().identity(identity).ca_certificate(ca);
    if let Some(domain) = domain {
        config = config.domain_name(domain);
    }
    Ok(config)
}
