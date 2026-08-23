# ADR-0022: Cosign signature verification for model artifacts

## Status

Accepted (2026-08-23)

## Context

Model artifacts are digest-pinned (ADR-0016): the URI names the exact
bytes, so substitution requires changing the pin, which is a reviewed
resource edit. What pinning alone cannot answer is *who produced* the
artifact - a compromised publishing pipeline can produce a correctly
pinned reference to a malicious model. ADR-0019 anticipated signatures;
the release pipeline already cosign-signs the routeD images themselves.

## Decision

Optional, key-based cosign verification for `oci://` artifacts:

- `ROUTED_ARTIFACT_COSIGN_PUB=<path to cosign.pub>` (ECDSA P-256 PEM, the
  key `cosign generate-key-pair` writes) makes signature verification
  **mandatory** for every `oci://` artifact the resolver fetches.
- Verification reads the signature manifest cosign stores at the
  `sha256-<digest>.sig` tag: each layer is one signature - the layer blob
  is the `SimpleSigning` payload, the
  `dev.cosignproject.cosign/signature` annotation the DER-encoded ECDSA
  signature over it. An artifact passes when any signature validates
  against the configured key **and** its payload names exactly the pinned
  manifest digest. The signature is checked before any artifact bytes are
  fetched.
- Fail closed everywhere: a configured-but-unreadable key poisons the
  trust root (every `oci://` fetch fails with a diagnostic rather than
  silently skipping verification); a missing signature manifest is a
  verification failure, not a fallback to pin-only trust.
- Scope: `oci://` only. `https://` and `file://` artifacts keep pin-only
  trust - they have no standardised signature channel, and the threat
  model documents that difference. Keyless (Fulcio / Rekor) verification
  is out of scope: it would put a public trust root and transparency-log
  lookups on the router's startup path.
- Verification happens at pull time; cache hits are not re-verified (the
  cache re-checks content digests only). The cache directory remains
  pod-local, per the threat model.

## Consequences

- A tenant can require that every OCI-distributed model was signed by
  their publishing key, closing the "correct pin, malicious publisher"
  path for the strongest artifact channel.
- Two small new dependencies (`p256`, `base64ct`), both pure Rust.
- Signing stays in the publisher's pipeline (`cosign sign --key ...`);
  routeD only verifies.
