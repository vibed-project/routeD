# Architecture Decision Records

An ADR records a decision that establishes or alters a contract another
component depends on: CRD shapes, the Decision JSON, header semantics, the
snapshot format, extension seams, evaluation order, toolchain policy. Ordinary
implementation details do not get an ADR.

## Conventions

- One decision per file, named `NNNN-kebab-title.md`, zero-padded to four
  digits, numbered sequentially. Numbers are never reused.
- Headings, in order: `# ADR-NNNN: Title`, `## Status`, `## Context`,
  `## Decision`, `## Consequences`, `## Alternatives considered`.
- Status is `Draft`, `Accepted`, or `Superseded by ADR-XXXX`.
- Reversals get a new ADR; the old one's Status line is updated. ADRs are never
  retro-edited beyond the Status line and typo fixes.

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](0001-decision-layer-not-gateway.md) | routeD is a decision layer, not a gateway | Accepted |
| [0002](0002-rust-and-onnx.md) | Rust for all runtime components; ONNX for local inference | Accepted |
| [0003](0003-security-before-cost.md) | Security constraints are evaluated before cost optimization | Accepted |
| [0004](0004-extension-seams.md) | Extension seams | Accepted |
| [0005](0005-workspace-layout-and-containerized-toolchain.md) | Cargo workspace layout and containerized toolchain | Accepted |
| [0006](0006-classifier-seam-and-degradation.md) | Classifier seam and degradation semantics | Accepted |
| [0007](0007-header-trust-model.md) | Header trust model: untrusted headers only restrict | Accepted |
| [0008](0008-single-source-compiler-and-snapshot-hash.md) | Single-source policy compiler and canonical snapshot hash | Accepted |
| [0009](0009-cost-model-currency-and-token-estimation.md) | Cost model, currency and token estimation | Accepted |
| [0010](0010-policy-precedence.md) | Policy precedence | Accepted |
| [0011](0011-goldens-as-examples.md) | Golden decisions live in examples/ | Accepted |
| [0012](0012-inline-forwarder-streaming.md) | Inline forwarder and streaming | Accepted |
| [0013](0013-telemetry-schema.md) | Telemetry schema | Accepted |
| [0014](0014-operator-reconciliation-and-distribution.md) | Operator reconciliation and snapshot distribution | Accepted |
| [0015](0015-admission-validation.md) | Admission validation | Accepted |
| [0016](0016-artifact-resolution-and-onnx-contract.md) | Artifact resolution and the ONNX model contract | Accepted |
| [0017](0017-extproc-processing-contract.md) | ext_proc processing contract | Accepted |
| [0018](0018-feedback-records-and-learned-router.md) | Feedback records and the learned router contract | Accepted |
| [0019](0019-supply-chain-and-release.md) | Supply chain and release engineering | Accepted |
