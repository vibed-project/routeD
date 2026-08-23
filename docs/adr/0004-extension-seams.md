# ADR-0004: Extension seams

## Status

Accepted

## Context

Integrations and alternative implementations (different classifiers,
feedback exporters, snapshot sources, telemetry backends) must plug into
routeD without forking core code, and their absence must never degrade the
router: every seam ships with a working default in this repository.

## Decision

- Extension points are Rust traits defined **in core**, each with a working
  default implementation shipped in this repository: `Classifier`
  (heuristic, stub, http, onnx), `QualityPredictor` (tier priors by
  default, learned router optional), `FeedbackSink` (null and JSONL), the
  router's snapshot sources (files, compiled file, operator gRPC), and the
  telemetry exporter configuration (Prometheus always, OTLP optional).
- Core never imports an extension. Extensions are separate crates or
  external services wired through configuration (`RouterProfile`, CLI
  flags, Helm values), never through patches to core.
- A seam never disables a core feature, and changing a seam's contract
  requires updating this ADR's seam list.
- The CRD group `routed.io/v1alpha1` is fully open; extensions use their
  own API groups and never extend these types.
- `scripts/check-crate-boundary.sh` keeps the pure crates free of runtime
  dependencies.

## Consequences

- Contributors can reason about the full system from this repository alone.
- Every seam has at least two implementations in-tree or in tests, which
  keeps the contracts honest.

## Alternatives considered

- **Feature flags for alternative code paths in core.** Rejected: couples
  core to every extension and invites forks.
- **Plugin ABI / dynamic loading.** Rejected for v0.x: Rust has no stable
  ABI; traits plus configuration are sufficient.
