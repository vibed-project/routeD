// SPDX-License-Identifier: Apache-2.0
//! Cosign signature verification for `oci://` artifacts (ADR-0022).
//!
//! Key-based verification of the signature manifest cosign stores at the
//! `sha256-<digest>.sig` tag next to the artifact: each layer is one
//! signature - the layer blob is the `SimpleSigning` payload naming the
//! signed manifest digest, and the
//! `dev.cosignproject.cosign/signature` annotation carries the ECDSA P-256
//! signature (ASN.1 DER, base64) over those payload bytes. Verification
//! passes when any signature (a) validates against the configured public
//! key and (b) names exactly the pinned manifest digest.
//!
//! Keyless (Fulcio / Rekor) verification is out of scope: it would pull a
//! trust root and network dependencies into the router's startup path.

use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};

use crate::FetchError;

/// The annotation cosign stores the signature under.
const SIGNATURE_ANNOTATION: &str = "dev.cosignproject.cosign/signature";

/// A configured cosign public key (or a poisoned trust root that fails
/// every verification - configured but unusable keys must fail closed).
pub struct CosignVerifier {
    key: Option<VerifyingKey>,
}

impl CosignVerifier {
    /// Parse a cosign public key (PEM, ECDSA P-256 - the `cosign.pub` that
    /// `cosign generate-key-pair` writes).
    ///
    /// # Errors
    /// When the PEM is not a valid P-256 public key.
    pub fn from_pem(pem: &str) -> Result<Self, FetchError> {
        use p256::pkcs8::DecodePublicKey as _;
        let key = VerifyingKey::from_public_key_pem(pem).map_err(|e| FetchError::Fetch {
            uri: "<cosign public key>".to_owned(),
            reason: format!("not a valid ECDSA P-256 public key: {e}"),
        })?;
        Ok(Self { key: Some(key) })
    }

    /// A verifier that rejects everything: the trust root was configured
    /// but could not be loaded.
    #[must_use]
    pub fn poisoned() -> Self {
        Self { key: None }
    }

    /// The registry tag cosign stores the signature manifest under.
    #[must_use]
    pub fn sig_tag(manifest_digest_hex: &str) -> String {
        format!("sha256-{manifest_digest_hex}.sig")
    }

    /// Verify the signature manifest for the pinned artifact manifest.
    /// `fetch_blob` fetches a payload blob by its `sha256:<hex>` digest and
    /// must itself verify the content hash (the OCI fetcher already does).
    ///
    /// # Errors
    /// [`FetchError::SignatureVerification`] when no signature validates.
    pub fn verify(
        &self,
        uri: &str,
        manifest_digest_hex: &str,
        sig_manifest: &serde_json::Value,
        fetch_blob: &mut dyn FnMut(&str) -> Result<Vec<u8>, FetchError>,
    ) -> Result<(), FetchError> {
        let fail = |reason: String| FetchError::SignatureVerification {
            uri: uri.to_owned(),
            reason,
        };
        let Some(key) = &self.key else {
            return Err(fail(
                "cosign trust root configured but unusable (see startup logs)".to_owned(),
            ));
        };
        let layers = sig_manifest
            .get("layers")
            .and_then(|l| l.as_array())
            .ok_or_else(|| fail("signature manifest has no layers".to_owned()))?;
        if layers.is_empty() {
            return Err(fail("signature manifest has no signatures".to_owned()));
        }
        let mut last = "no signature checked".to_owned();
        for layer in layers {
            match Self::verify_layer(key, manifest_digest_hex, layer, fetch_blob) {
                Ok(()) => return Ok(()),
                Err(reason) => last = reason,
            }
        }
        Err(fail(format!(
            "none of {} signature(s) verified (last: {last})",
            layers.len()
        )))
    }

    fn verify_layer(
        key: &VerifyingKey,
        manifest_digest_hex: &str,
        layer: &serde_json::Value,
        fetch_blob: &mut dyn FnMut(&str) -> Result<Vec<u8>, FetchError>,
    ) -> Result<(), String> {
        let sig_b64 = layer
            .get("annotations")
            .and_then(|a| a.get(SIGNATURE_ANNOTATION))
            .and_then(|s| s.as_str())
            .ok_or_else(|| "layer has no signature annotation".to_owned())?;
        let sig_der = {
            use base64ct::Encoding as _;
            base64ct::Base64::decode_vec(sig_b64.trim())
                .map_err(|e| format!("signature is not base64: {e}"))?
        };
        let signature =
            Signature::from_der(&sig_der).map_err(|e| format!("signature is not DER: {e}"))?;
        let payload_digest = layer
            .get("digest")
            .and_then(|d| d.as_str())
            .ok_or_else(|| "signature layer has no digest".to_owned())?;
        let payload = fetch_blob(payload_digest).map_err(|e| e.to_string())?;
        key.verify(&payload, &signature)
            .map_err(|_| "signature does not verify against the configured key".to_owned())?;
        // Only after the signature is valid do we trust the payload's claim.
        let claimed: serde_json::Value =
            serde_json::from_slice(&payload).map_err(|e| format!("payload is not JSON: {e}"))?;
        let signed_digest = claimed
            .pointer("/critical/image/docker-manifest-digest")
            .and_then(|d| d.as_str())
            .ok_or_else(|| "payload names no manifest digest".to_owned())?;
        if signed_digest != format!("sha256:{manifest_digest_hex}") {
            return Err(format!(
                "signature is for {signed_digest}, not the pinned sha256:{manifest_digest_hex}"
            ));
        }
        Ok(())
    }
}
