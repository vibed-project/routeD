// SPDX-License-Identifier: Apache-2.0
//! Cosign verification semantics (ADR-0022) against signatures constructed
//! exactly as cosign stores them: `SimpleSigning` payload blobs signed with
//! ECDSA P-256, DER-encoded, base64 in the layer annotation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::EncodePublicKey as _;
use routed_artifact::FetchError;
use routed_artifact::cosign::CosignVerifier;
use sha2::Digest as _;

struct SignedFixture {
    verifier: CosignVerifier,
    manifest: serde_json::Value,
    blobs: BTreeMap<String, Vec<u8>>,
    manifest_digest_hex: String,
}

fn b64(bytes: &[u8]) -> String {
    use base64ct::Encoding as _;
    base64ct::Base64::encode_string(bytes)
}

/// Build a signature manifest for `manifest_digest_hex` the way cosign does.
fn fixture(signing_key: &SigningKey, pub_pem: &str, signed_digest: &str) -> SignedFixture {
    let manifest_digest_hex = "a".repeat(64);
    let payload = serde_json::json!({
        "critical": {
            "identity": { "docker-reference": "ghcr.io/example/model" },
            "image": { "docker-manifest-digest": signed_digest },
            "type": "cosign container image signature"
        },
        "optional": null
    })
    .to_string()
    .into_bytes();
    let signature: Signature = signing_key.sign(&payload);
    let payload_digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&payload)));
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "layers": [{
            "mediaType": "application/vnd.dev.cosign.simplesigning.v1+json",
            "digest": payload_digest,
            "size": payload.len(),
            "annotations": {
                "dev.cosignproject.cosign/signature": b64(signature.to_der().as_bytes()),
            }
        }]
    });
    let mut blobs = BTreeMap::new();
    blobs.insert(payload_digest, payload);
    SignedFixture {
        verifier: CosignVerifier::from_pem(pub_pem).unwrap(),
        manifest,
        blobs,
        manifest_digest_hex,
    }
}

fn keypair() -> (SigningKey, String) {
    let key = SigningKey::random(&mut rand_core::OsRng);
    let pem = key
        .verifying_key()
        .to_public_key_pem(p256::pkcs8::LineEnding::LF)
        .unwrap();
    (key, pem)
}

fn run(f: &SignedFixture) -> Result<(), FetchError> {
    let blobs = f.blobs.clone();
    let mut fetch = move |digest: &str| -> Result<Vec<u8>, FetchError> {
        blobs.get(digest).cloned().ok_or(FetchError::Fetch {
            uri: "oci://test".into(),
            reason: format!("no blob {digest}"),
        })
    };
    f.verifier.verify(
        "oci://test",
        &f.manifest_digest_hex,
        &f.manifest,
        &mut fetch,
    )
}

#[test]
fn a_valid_signature_for_the_pinned_digest_verifies() {
    let (key, pem) = keypair();
    let signed = format!("sha256:{}", "a".repeat(64));
    let f = fixture(&key, &pem, &signed);
    run(&f).unwrap();
}

#[test]
fn a_signature_for_a_different_digest_is_rejected() {
    let (key, pem) = keypair();
    // Signed digest differs from the pinned one: valid crypto, wrong claim.
    let signed = format!("sha256:{}", "b".repeat(64));
    let f = fixture(&key, &pem, &signed);
    let err = run(&f).unwrap_err().to_string();
    assert!(err.contains("not the pinned"), "{err}");
}

#[test]
fn a_signature_from_a_different_key_is_rejected() {
    let (key, _) = keypair();
    let (_, other_pub) = keypair();
    let signed = format!("sha256:{}", "a".repeat(64));
    let mut f = fixture(&key, &other_pub, &signed);
    f.verifier = CosignVerifier::from_pem(&other_pub).unwrap();
    let err = run(&f).unwrap_err().to_string();
    assert!(err.contains("does not verify"), "{err}");
}

#[test]
fn a_tampered_payload_is_rejected() {
    let (key, pem) = keypair();
    let signed = format!("sha256:{}", "a".repeat(64));
    let mut f = fixture(&key, &pem, &signed);
    // Flip a byte in the payload blob; its digest entry stays the same so
    // the tamper is caught by the signature, mirroring a registry that
    // serves consistent-but-forged content.
    let (digest, payload) = f
        .blobs
        .iter()
        .next()
        .map(|(k, v)| (k.clone(), v.clone()))
        .unwrap();
    let mut forged = payload;
    let i = forged.len() / 2;
    forged[i] ^= 1;
    f.blobs.insert(digest, forged);
    let err = run(&f).unwrap_err().to_string();
    assert!(err.contains("does not verify"), "{err}");
}

#[test]
fn empty_and_poisoned_trust_roots_fail_closed() {
    let (key, pem) = keypair();
    let signed = format!("sha256:{}", "a".repeat(64));
    let mut f = fixture(&key, &pem, &signed);
    f.manifest = serde_json::json!({ "schemaVersion": 2, "layers": [] });
    let err = run(&f).unwrap_err().to_string();
    assert!(err.contains("no signatures"), "{err}");

    let mut f = fixture(&key, &pem, &signed);
    f.verifier = CosignVerifier::poisoned();
    let err = run(&f).unwrap_err().to_string();
    assert!(err.contains("unusable"), "{err}");

    assert!(CosignVerifier::from_pem("not a key").is_err());
}
