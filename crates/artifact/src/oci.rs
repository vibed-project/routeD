// SPDX-License-Identifier: Apache-2.0
//! OCI registry artifact pull by manifest digest (ADR-0019).
//!
//! Trust chain: the URI pins the manifest digest; the fetched manifest bytes
//! must hash to it; the layer blob must hash to the digest the now-trusted
//! manifest names. Tags are never accepted. Multi-layer images are rejected:
//! a routeD model artifact is a single-layer OCI artifact.
//!
//! Auth: anonymous, with the standard `WWW-Authenticate: Bearer` token flow
//! on 401 (anonymous token request; covers public GHCR / Docker Hub).
//! Transport is HTTPS except for localhost registries (tests, kind).

use std::io::Read as _;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{FetchError, MAX_ARTIFACT_BYTES};

const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

/// `registry/repository` split out of an `oci://` location.
struct Reference {
    base: String,
    repository: String,
}

fn parse_location(uri: &str, location: &str) -> Result<Reference, FetchError> {
    let rest = location.trim_start_matches("oci://");
    let (host, repository) = rest.split_once('/').ok_or_else(|| FetchError::InvalidUri {
        uri: uri.to_owned(),
        reason: "oci:// needs <registry>/<repository>".to_owned(),
    })?;
    if repository.is_empty() {
        return Err(FetchError::InvalidUri {
            uri: uri.to_owned(),
            reason: "empty repository".to_owned(),
        });
    }
    let plain =
        host.starts_with("localhost") || host.starts_with("127.0.0.1") || host.starts_with("[::1]");
    let scheme = if plain { "http" } else { "https" };
    Ok(Reference {
        base: format!("{scheme}://{host}"),
        repository: repository.to_owned(),
    })
}

/// GET with optional bearer token; on 401, run the token dance once.
fn get(
    uri: &str,
    url: &str,
    accept: &str,
    token: &mut Option<String>,
) -> Result<Vec<u8>, FetchError> {
    let fetch_err = |reason: String| FetchError::Fetch {
        uri: uri.to_owned(),
        reason,
    };
    for attempt in 0..2 {
        let mut req = ureq::get(url).header("accept", accept);
        if let Some(t) = token.as_deref() {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(response) => {
                let mut body = Vec::new();
                response
                    .into_body()
                    .into_reader()
                    .take(MAX_ARTIFACT_BYTES)
                    .read_to_end(&mut body)
                    .map_err(|e| fetch_err(e.to_string()))?;
                return Ok(body);
            }
            Err(ureq::Error::StatusCode(401)) if attempt == 0 && token.is_none() => {
                // ureq surfaces the status but not the response headers here;
                // re-issue the request without erroring on status to read
                // WWW-Authenticate.
                let response = ureq::get(url)
                    .header("accept", accept)
                    .config()
                    .http_status_as_error(false)
                    .build()
                    .call()
                    .map_err(|e| fetch_err(e.to_string()))?;
                let challenge = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                *token = Some(anonymous_token(uri, &challenge)?);
            }
            Err(e) => return Err(fetch_err(e.to_string())),
        }
    }
    Err(fetch_err("authentication did not converge".to_owned()))
}

/// `Bearer realm="...",service="...",scope="..."` -> anonymous token.
fn anonymous_token(uri: &str, challenge: &str) -> Result<String, FetchError> {
    let fetch_err = |reason: String| FetchError::Fetch {
        uri: uri.to_owned(),
        reason,
    };
    let field = |name: &str| {
        challenge
            .split([',', ' '])
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .map(|v| v.trim_matches('"').to_owned())
    };
    let realm = field("realm").ok_or_else(|| {
        fetch_err(format!(
            "401 without a Bearer realm challenge: {challenge:?}"
        ))
    })?;
    let mut url = realm;
    let mut sep = '?';
    for key in ["service", "scope"] {
        if let Some(v) = field(key) {
            url.push(sep);
            url.push_str(key);
            url.push('=');
            url.push_str(&urlencode(&v));
            sep = '&';
        }
    }
    let body = ureq::get(&url)
        .call()
        .map_err(|e| fetch_err(e.to_string()))?
        .into_body()
        .read_to_string()
        .map_err(|e| fetch_err(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| fetch_err(e.to_string()))?;
    v.get("token")
        .or_else(|| v.get("access_token"))
        .and_then(|t| t.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| fetch_err("token endpoint returned no token".to_owned()))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Pull the single artifact layer named by the pinned manifest into `dest`.
pub(crate) fn fetch_verified(
    uri: &str,
    location: &str,
    manifest_digest_hex: &str,
    dest: &Path,
    cache_dir: &Path,
    cosign: Option<&crate::cosign::CosignVerifier>,
) -> Result<(), FetchError> {
    let fetch_err = |reason: String| FetchError::Fetch {
        uri: uri.to_owned(),
        reason,
    };
    let r = parse_location(uri, location)?;
    let mut token: Option<String> = None;

    // 1. Manifest by digest, verified against the pin.
    let manifest_url = format!(
        "{}/v2/{}/manifests/sha256:{manifest_digest_hex}",
        r.base, r.repository
    );
    let manifest_bytes = get(uri, &manifest_url, MANIFEST_ACCEPT, &mut token)?;
    let actual = hex::encode(Sha256::digest(&manifest_bytes));
    if actual != manifest_digest_hex {
        return Err(FetchError::DigestMismatch {
            uri: uri.to_owned(),
            expected: manifest_digest_hex.to_owned(),
            actual,
        });
    }

    // 1b. When a cosign trust root is configured, the pinned manifest must
    // carry a valid signature before any artifact bytes are fetched
    // (ADR-0022). The signature manifest is fetched by its well-known tag;
    // its integrity comes from the signatures themselves, not the tag.
    if let Some(verifier) = cosign {
        let sig_tag = crate::cosign::CosignVerifier::sig_tag(manifest_digest_hex);
        let sig_url = format!("{}/v2/{}/manifests/{sig_tag}", r.base, r.repository);
        let sig_manifest_bytes = get(uri, &sig_url, MANIFEST_ACCEPT, &mut token).map_err(|e| {
            FetchError::SignatureVerification {
                uri: uri.to_owned(),
                reason: format!("signature manifest {sig_tag} not fetchable: {e}"),
            }
        })?;
        let sig_manifest: serde_json::Value =
            serde_json::from_slice(&sig_manifest_bytes).map_err(|e| {
                FetchError::SignatureVerification {
                    uri: uri.to_owned(),
                    reason: format!("signature manifest is not JSON: {e}"),
                }
            })?;
        let base = r.base.clone();
        let repository = r.repository.clone();
        let mut fetch_blob = |digest: &str| -> Result<Vec<u8>, FetchError> {
            let hex_digest = digest.strip_prefix("sha256:").ok_or_else(|| {
                fetch_err(format!("signature payload digest {digest:?} is not sha256"))
            })?;
            let blob_url = format!("{base}/v2/{repository}/blobs/{digest}");
            let blob = get(uri, &blob_url, "application/octet-stream", &mut token)?;
            let actual = hex::encode(Sha256::digest(&blob));
            if actual != hex_digest {
                return Err(FetchError::DigestMismatch {
                    uri: uri.to_owned(),
                    expected: hex_digest.to_owned(),
                    actual,
                });
            }
            Ok(blob)
        };
        verifier.verify(uri, manifest_digest_hex, &sig_manifest, &mut fetch_blob)?;
        tracing::info!(uri, "cosign signature verified");
    }

    // 2. The single layer the trusted manifest names.
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(|e| fetch_err(e.to_string()))?;
    let layers = manifest
        .get("layers")
        .and_then(|l| l.as_array())
        .ok_or_else(|| fetch_err("manifest has no layers".to_owned()))?;
    if layers.len() != 1 {
        return Err(fetch_err(format!(
            "expected a single-layer artifact, found {} layers (ADR-0019)",
            layers.len()
        )));
    }
    let layer_digest = layers[0]
        .get("digest")
        .and_then(|d| d.as_str())
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| fetch_err("layer has no sha256 digest".to_owned()))?
        .to_owned();

    // 3. The blob, verified against the layer digest.
    let blob_url = format!("{}/v2/{}/blobs/sha256:{layer_digest}", r.base, r.repository);
    let blob = get(uri, &blob_url, "application/octet-stream", &mut token)?;
    let actual = hex::encode(Sha256::digest(&blob));
    if actual != layer_digest {
        return Err(FetchError::DigestMismatch {
            uri: uri.to_owned(),
            expected: layer_digest,
            actual,
        });
    }

    let dir = dest.parent().unwrap_or(cache_dir);
    let io_err = |path: &Path| {
        let path = path.to_owned();
        move |source: std::io::Error| FetchError::Io { path, source }
    };
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &blob).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, dest).map_err(io_err(dest))?;
    // Record the layer digest so cache hits can re-verify the file content
    // (the cache key is the manifest digest, which the file does not hash to).
    std::fs::write(dest.with_extension("layer"), &layer_digest).map_err(io_err(dest))?;
    tracing::info!(uri, bytes = blob.len(), path = %dest.display(), "oci artifact pulled and verified");
    Ok(())
}
