// SPDX-License-Identifier: Apache-2.0
//! Model artifact resolution (ADR-0016): digest-pinned `https://` / `http://`
//! fetching and `file://` references, verified into a content-addressed
//! cache. `oci://` is reserved and rejected until the artifact-signing work.
//!
//! Artifacts are supply-chain sensitive: a swapped classifier can silently
//! loosen routing. Remote fetches therefore always require a pinned
//! `@sha256:<hex>` suffix, cache hits are re-verified before reuse, and
//! downloads only enter the cache after their digest checks out.

mod oci;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Resolution failure. Every variant is terminal for the given URI.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The URI does not parse or uses an unsupported scheme.
    #[error("invalid artifact uri {uri:?}: {reason}")]
    InvalidUri {
        /// The offending URI.
        uri: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The fetched or referenced bytes do not match the pinned digest.
    #[error("digest mismatch for {uri:?}: expected sha256:{expected}, got sha256:{actual}")]
    DigestMismatch {
        /// The artifact URI.
        uri: String,
        /// Pinned digest (hex).
        expected: String,
        /// Computed digest (hex).
        actual: String,
    },
    /// Network or HTTP failure.
    #[error("fetching {uri:?} failed: {reason}")]
    Fetch {
        /// The artifact URI.
        uri: String,
        /// Underlying error.
        reason: String,
    },
    /// Filesystem failure (cache or `file://` source).
    #[error("io error for {path:?}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Maximum artifact size accepted (defensive bound; classifier models are
/// tens of megabytes).
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A parsed artifact reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRef {
    /// Fetchable location without the digest suffix.
    pub location: String,
    /// URI scheme (`https`, `http`, `file`).
    pub scheme: Scheme,
    /// Pinned sha256 digest (lowercase hex), when present.
    pub digest: Option<String>,
}

/// Supported schemes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// TLS-transported fetch.
    Https,
    /// Plaintext fetch (digest makes it tamper-evident; still warned about).
    Http,
    /// Local file.
    File,
    /// OCI registry artifact, pulled by manifest digest (ADR-0019).
    Oci,
}

/// Parse an artifact URI (`https://...@sha256:<hex>`, `file:///...`).
///
/// # Errors
/// [`FetchError::InvalidUri`] for unknown schemes, a missing digest on a
/// remote URI, or a malformed digest.
pub fn parse(uri: &str) -> Result<ArtifactRef, FetchError> {
    let invalid = |reason: &str| FetchError::InvalidUri {
        uri: uri.to_owned(),
        reason: reason.to_owned(),
    };
    let scheme = if uri.starts_with("https://") {
        Scheme::Https
    } else if uri.starts_with("http://") {
        Scheme::Http
    } else if uri.starts_with("file://") {
        Scheme::File
    } else if uri.starts_with("oci://") {
        Scheme::Oci
    } else {
        return Err(invalid("expected oci://, https://, http:// or file://"));
    };
    let (location, digest) = match uri.rsplit_once("@sha256:") {
        Some((loc, hex)) => {
            if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(invalid("digest must be @sha256:<64 hex characters>"));
            }
            (loc.to_owned(), Some(hex.to_ascii_lowercase()))
        }
        None => (uri.to_owned(), None),
    };
    if digest.is_none() && scheme != Scheme::File {
        return Err(invalid(
            "remote artifacts require @sha256:<hex> pinning (ADR-0016); oci:// tags are not accepted",
        ));
    }
    Ok(ArtifactRef {
        location,
        scheme,
        digest,
    })
}

/// Content-addressed artifact cache.
pub struct Resolver {
    cache_dir: PathBuf,
}

impl Resolver {
    /// Resolver over an explicit cache directory.
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Resolver over `$ROUTED_MODEL_CACHE`, `~/.cache/routed/models`, or
    /// `/var/cache/routed/models` in that order.
    #[must_use]
    pub fn from_env() -> Self {
        let dir = std::env::var_os("ROUTED_MODEL_CACHE").map_or_else(
            || {
                std::env::var_os("HOME").map_or_else(
                    || PathBuf::from("/var/cache/routed/models"),
                    |home| PathBuf::from(home).join(".cache/routed/models"),
                )
            },
            PathBuf::from,
        );
        Self::new(dir)
    }

    /// The cache directory.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Resolve a URI to a local file, fetching and verifying as needed.
    ///
    /// # Errors
    /// See [`FetchError`].
    pub fn resolve(&self, uri: &str) -> Result<PathBuf, FetchError> {
        let r = parse(uri)?;
        match r.scheme {
            Scheme::File => {
                let path = PathBuf::from(r.location.trim_start_matches("file://"));
                if let Some(expected) = &r.digest {
                    let actual = digest_of_file(&path)?;
                    if &actual != expected {
                        return Err(FetchError::DigestMismatch {
                            uri: uri.to_owned(),
                            expected: expected.clone(),
                            actual,
                        });
                    }
                }
                Ok(path)
            }
            Scheme::Https | Scheme::Http | Scheme::Oci => {
                if r.scheme == Scheme::Http {
                    tracing::warn!(uri, "artifact fetched over plaintext http (digest-pinned)");
                }
                let expected = r.digest.as_deref().unwrap_or_default();
                let cached = self.cache_dir.join("sha256").join(expected);
                if cached.is_file() {
                    let actual = digest_of_file(&cached)?;
                    if actual == expected && r.scheme != Scheme::Oci {
                        return Ok(cached);
                    }
                    // For oci:// the cache key is the manifest digest, not the
                    // file's own hash; a present file is trusted as-is only if
                    // it was written by us (atomic rename), so re-verify via
                    // the recorded layer digest file.
                    if r.scheme == Scheme::Oci {
                        if let Ok(layer) = std::fs::read_to_string(cached.with_extension("layer")) {
                            if digest_of_file(&cached)? == layer.trim() {
                                return Ok(cached);
                            }
                        }
                    }
                    tracing::warn!(uri, "cached artifact failed re-verification; refetching");
                }
                if r.scheme == Scheme::Oci {
                    oci::fetch_verified(uri, &r.location, expected, &cached, &self.cache_dir)?;
                } else {
                    self.fetch_verified(uri, &r.location, expected, &cached)?;
                }
                Ok(cached)
            }
        }
    }

    fn fetch_verified(
        &self,
        uri: &str,
        location: &str,
        expected: &str,
        dest: &Path,
    ) -> Result<(), FetchError> {
        let fetch_err = |reason: String| FetchError::Fetch {
            uri: uri.to_owned(),
            reason,
        };
        let io_err = |path: &Path| {
            let path = path.to_owned();
            move |source: std::io::Error| FetchError::Io { path, source }
        };
        let response = ureq::get(location)
            .call()
            .map_err(|e| fetch_err(e.to_string()))?;
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_ARTIFACT_BYTES)
            .read_to_end(&mut body)
            .map_err(|e| fetch_err(e.to_string()))?;
        let actual = hex::encode(Sha256::digest(&body));
        if actual != expected {
            return Err(FetchError::DigestMismatch {
                uri: uri.to_owned(),
                expected: expected.to_owned(),
                actual,
            });
        }
        let dir = dest.parent().unwrap_or(&self.cache_dir);
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, &body).map_err(io_err(&tmp))?;
        std::fs::rename(&tmp, dest).map_err(io_err(dest))?;
        tracing::info!(uri, bytes = body.len(), path = %dest.display(), "artifact fetched and verified");
        Ok(())
    }
}

fn digest_of_file(path: &Path) -> Result<String, FetchError> {
    let bytes = std::fs::read(path).map_err(|source| FetchError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn parses_and_rejects_uris() {
        let r = parse(&format!(
            "https://models.example/m.onnx@sha256:{}",
            "a".repeat(64)
        ))
        .unwrap();
        assert_eq!(r.scheme, Scheme::Https);
        assert_eq!(r.location, "https://models.example/m.onnx");
        assert_eq!(r.digest.as_deref(), Some("a".repeat(64).as_str()));

        assert!(
            parse("https://models.example/m.onnx").is_err(),
            "digest required"
        );
        assert!(parse("oci://ghcr.io/x@sha256:00").is_err(), "oci reserved");
        assert!(parse("ftp://x").is_err(), "unknown scheme");
        assert!(
            parse(&format!("https://x/m@sha256:{}", "z".repeat(64))).is_err(),
            "non-hex digest"
        );
        let f = parse("file:///models/m.onnx").unwrap();
        assert_eq!(f.scheme, Scheme::File);
        assert!(f.digest.is_none(), "file needs no digest");
    }

    #[test]
    fn file_uri_verifies_optional_digest() {
        let dir = std::env::temp_dir().join(format!("routed-artifact-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.bin");
        std::fs::write(&path, b"model-bytes").unwrap();
        let resolver = Resolver::new(dir.join("cache"));

        let ok = resolver
            .resolve(&format!(
                "file://{}@sha256:{}",
                path.display(),
                sha(b"model-bytes")
            ))
            .unwrap();
        assert_eq!(ok, path);
        let err = resolver
            .resolve(&format!(
                "file://{}@sha256:{}",
                path.display(),
                "0".repeat(64)
            ))
            .unwrap_err();
        assert!(matches!(err, FetchError::DigestMismatch { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Minimal single-request HTTP server for exercising the fetch path.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{addr}/model.onnx")
    }

    #[test]
    fn fetches_verifies_and_caches() {
        let dir =
            std::env::temp_dir().join(format!("routed-artifact-cache-{}", std::process::id()));
        let resolver = Resolver::new(dir.clone());
        let body = b"onnx-model-payload".to_vec();
        let uri = format!("{}@sha256:{}", serve_once(body.clone()), sha(&body));

        let path = resolver.resolve(&uri).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        // Second resolve is served from the cache (the server accepted only
        // one request, so a refetch would fail).
        let again = resolver.resolve(&uri).unwrap();
        assert_eq!(again, path);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Minimal multi-request OCI registry mock: routes by path, optionally
    /// demanding the anonymous Bearer token dance first.
    fn serve_registry(
        manifest: Vec<u8>,
        blob: Vec<u8>,
        blob_digest: String,
        require_auth: bool,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let manifest_digest = sha(&manifest);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 8192];
                let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                let authed = req.contains("authorization: Bearer test-token")
                    || req.contains("Authorization: Bearer test-token");
                let respond = |stream: &mut std::net::TcpStream,
                               status: &str,
                               headers: &str,
                               body: &[u8]| {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                };
                if path.starts_with("/token") {
                    respond(
                        &mut stream,
                        "200 OK",
                        "content-type: application/json\r\n",
                        br#"{"token":"test-token"}"#,
                    );
                } else if require_auth && !authed {
                    let challenge = format!(
                        "www-authenticate: Bearer realm=\"http://{addr}/token\",service=\"reg\",scope=\"repository:models/m:pull\"\r\n"
                    );
                    respond(&mut stream, "401 Unauthorized", &challenge, b"{}");
                } else if path == format!("/v2/models/m/manifests/sha256:{manifest_digest}") {
                    respond(
                        &mut stream,
                        "200 OK",
                        "content-type: application/vnd.oci.image.manifest.v1+json\r\n",
                        &manifest,
                    );
                } else if path == format!("/v2/models/m/blobs/sha256:{blob_digest}") {
                    respond(
                        &mut stream,
                        "200 OK",
                        "content-type: application/octet-stream\r\n",
                        &blob,
                    );
                } else {
                    respond(&mut stream, "404 Not Found", "", b"{}");
                    break; // unexpected path ends the mock
                }
            }
        });
        (format!("127.0.0.1:{}", addr.port()), handle)
    }

    fn oci_fixture(layers: usize) -> (Vec<u8>, Vec<u8>, String) {
        let blob = b"onnx-model-via-oci".to_vec();
        let blob_digest = sha(&blob);
        let layer = serde_json::json!({
            "mediaType": "application/octet-stream",
            "digest": format!("sha256:{blob_digest}"),
            "size": blob.len()
        });
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": { "mediaType": "application/vnd.oci.empty.v1+json", "digest": "sha256:0000", "size": 2 },
            "layers": vec![layer; layers],
        });
        (serde_json::to_vec(&manifest).unwrap(), blob, blob_digest)
    }

    #[test]
    fn oci_pull_verifies_manifest_and_layer() {
        let (manifest, blob, blob_digest) = oci_fixture(1);
        let manifest_digest = sha(&manifest);
        let (addr, _h) = serve_registry(manifest, blob.clone(), blob_digest, false);
        let dir = std::env::temp_dir().join(format!("routed-oci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resolver = Resolver::new(dir.clone());
        let uri = format!("oci://{addr}/models/m@sha256:{manifest_digest}");
        let path = resolver.resolve(&uri).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), blob);
        // Cache hit: no network (the mock would 404 unexpected paths).
        let again = resolver.resolve(&uri).unwrap();
        assert_eq!(again, path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oci_pull_runs_the_anonymous_token_dance() {
        let (manifest, blob, blob_digest) = oci_fixture(1);
        let manifest_digest = sha(&manifest);
        let (addr, _h) = serve_registry(manifest, blob.clone(), blob_digest, true);
        let dir = std::env::temp_dir().join(format!("routed-oci-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resolver = Resolver::new(dir.clone());
        let uri = format!("oci://{addr}/models/m@sha256:{manifest_digest}");
        let path = resolver.resolve(&uri).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), blob);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oci_rejects_multi_layer_and_wrong_digest_and_tags() {
        // Multi-layer artifact.
        let (manifest, blob, blob_digest) = oci_fixture(2);
        let manifest_digest = sha(&manifest);
        let (addr, _h) = serve_registry(manifest, blob, blob_digest, false);
        let dir = std::env::temp_dir().join(format!("routed-oci-bad-{}", std::process::id()));
        let resolver = Resolver::new(dir.clone());
        let err = resolver
            .resolve(&format!("oci://{addr}/models/m@sha256:{manifest_digest}"))
            .unwrap_err();
        assert!(err.to_string().contains("single-layer"), "{err}");
        // Digest pin mismatch: the mock 404s the unknown manifest.
        let (other_manifest, other_blob, other_digest) = oci_fixture(1);
        let (addr2, _h2) = serve_registry(other_manifest, other_blob, other_digest, false);
        let err = resolver
            .resolve(&format!("oci://{addr2}/models/m@sha256:{}", "0".repeat(64)))
            .unwrap_err();
        assert!(matches!(err, FetchError::Fetch { .. }), "{err}");
        // Tags are never accepted.
        assert!(parse("oci://ghcr.io/models/m:latest").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn digest_mismatch_rejects_and_caches_nothing() {
        let dir = std::env::temp_dir().join(format!("routed-artifact-bad-{}", std::process::id()));
        let resolver = Resolver::new(dir.clone());
        let uri = format!("{}@sha256:{}", serve_once(b"evil".to_vec()), "0".repeat(64));
        let err = resolver.resolve(&uri).unwrap_err();
        assert!(matches!(err, FetchError::DigestMismatch { .. }), "{err}");
        assert!(
            !dir.join("sha256").join("0".repeat(64)).exists(),
            "mismatched bytes must not enter the cache"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
