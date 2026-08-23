# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); routeD is pre-1.0, so
minor versions may still break APIs.

## [Unreleased]

Nothing yet.

## [0.1.2] - 2026-08-23

### Added

- Mutual TLS for snapshot distribution (ADR-0021): `routed-proto::tls`
  builds server/client configs from a `tls.crt`/`tls.key`/`ca.crt`
  directory (Kubernetes TLS-secret layout plus the peer CA). The operator
  gains `--snapshot-tls-dir` (requires CA-signed client certificates);
  routed gains `--snapshot-tls-dir` / `--snapshot-tls-domain`. Plain TCP
  remains the default.

### Changed

- Dependency bump (docker/setup-buildx-action v4).

## [0.1.1] - 2026-08-23

### Added

- Pluggable authentication seam for the decision APIs (ADR-0020): an
  `Authenticator` trait in `routed-ingress-inline` with an allow-all
  default, composed via `AppState::with_authenticator`. `/v1/decide` and
  `/v1/feedback` enforce it before any body read; denials use the standard
  OpenAI error envelope (401/403). Default behaviour is unchanged.
- Chart: `extraContainers` and `extraVolumes` passthrough on the router
  Deployment, for sidecars that share the pod (for example log shippers
  reading the feedback journal).

### Changed

- Release pipeline: build-record artifact upload disabled and the publish
  job's artifact download scoped to what it publishes, fixing the
  transient publish failures seen during v0.1.0.
- Dependency bumps (tokenizers 0.23, actions/checkout v7,
  softprops/action-gh-release v3, upload/download-artifact v7).

## [0.1.0] - 2026-08-23

First release: the complete decision layer described in the README - both
ingress modes, the operator with admission validation, local ONNX
inference, the learning loop, and the offline CLI. Highlights below; the
threat model lives in `docs/threat-model.md` and the supply-chain story in
ADR-0019 (signed images, SBOMs, digest-pinned artifacts including
`oci://` pulls).

### Added
- Phase 6 learning loop: `--feedback-dir` persists two append-only JSONL
  streams (ADR-0018) - the decision journal (closed-vocabulary findings and
  routing facts, never request content) and accepted `POST /v1/feedback`
  bodies - via a bounded channel that never blocks the request path
  (`crates/feedback`, `FeedbackSink` seam; Helm `feedback.enabled`). The
  learned router closes the loop: `trainer/` (`uv run routed-train`, numpy
  only) joins the streams, featurises with the shared `routed-features/1`
  layout, fits a logistic model and exports the ONNX contract (`features`
  in, `quality`/`confidence` out) that `OnnxQualityPredictor` (feature
  `onnx`) loads from `learnedRouter.uri` into the engine's
  `QualityPredictor` seam - predictions only refine `predictedQuality`,
  gated per policy by `minConfidence`. `routedctl validate` gains
  `--emit-snapshot` (the trainer's tier-feature source, single compiler),
  and `routedctl simulate` replays JSONL request logs offline into
  outcome / tier / cost / block summaries. Every routedctl subcommand is
  now implemented.
- Phase 5 ext_proc ingress: `routed serve --mode extproc` joins Envoy-based
  gateways as an `ext_proc` v3 external processor (ADR-0017), reusing the
  inline decision pipeline over shared state: buffered request bodies are
  decided, `ROUTE` mutates the body in place, `BLOCK` and dry-run become
  immediate responses with the shared OpenAI envelope, inbound `x-routed-*`
  headers are stripped even for pass-through traffic (skipped via
  `mode_override`), and the caller gets `X-Routed-*` response headers in the
  response-headers phase. Response bodies never pass through routeD.
  Generated protos come from the `envoy-types` crate; in-process protocol
  tests drive the generated Envoy client against the real tonic server, and
  the kind e2e gains a real-Envoy scenario. Gateway guides land under
  `docs/integration/` (LiteLLM, Envoy AI Gateway, agentgateway / kgateway,
  Kong).
- Phase 4 local inference: `crates/artifact` resolves digest-pinned model
  artifacts (`https://...@sha256:` mandatory pinning, `file://`, `oci://`
  reserved) into a content-addressed cache with atomic verified writes;
  `routedctl models pull` pre-warms it. The ONNX classifier (feature
  `onnx`, ADR-0016) runs a multi-head encoder (task / complexity /
  sensitivity / risk) through `ort` in load-dynamic mode with the
  `tokenizers` crate, composing with the heuristics so the model can only
  tighten findings (PII spans stay heuristic, risk = max(model,
  heuristics)). `RouterProfile.spec.classifier.tokenizerUri` joins the CRD;
  `type: onnx` without both artifact URIs is a compile error. `make onnx`
  fetches the Microsoft `libonnxruntime.so` and runs the feature's clippy /
  tests / p95 < 30 ms gate; criterion benches cover the classifier.
  Committed fixtures (model + tokenizer) are regenerable via
  `trainer/scripts/make_classifier_fixture.py`. Dedicated PII / injection
  detector models are deferred (ADR-0016).
- Validating admission webhook (phase 3 fast-follow, ADR-0015): the operator
  serves `POST /validate` over TLS, validating CRD writes with the same
  compiler as the reconciler against current cluster state with the incoming
  object substituted. Denials carry only error diagnostics attributed to the
  written object; warnings return as admission warnings. Fail-open by
  default (`operator.webhook.failurePolicy=Ignore`); the chart generates a
  self-signed certificate on first install and reuses it across upgrades so
  the `caBundle` and the served certificate always agree. The kind e2e
  verifies a broken `RoutingPolicy` is denied with the compiler diagnostic.
- Phase 3 operator: `routed-operator` reconciles `ModelTier`, `DataClass`,
  `RoutingPolicy` and `RouterProfile` cluster-wide into one compiled snapshot
  per change (`routed-policy`, unchanged), distributed to `routed` over a new
  `SnapshotService` gRPC stream (`crates/proto`) and, as a fallback, a
  `ConfigMap` the Helm chart mounts as a file. `status.conditions` (`Ready`)
  and `RoutingPolicy.status.compiledHash` are written from compile
  diagnostics; `coordination.k8s.io` `Lease`-based leader election gates
  status/`ConfigMap` writes without gating gRPC serving, so every replica
  answers watchers independently. `routed serve` gains `--snapshot-addr`
  (gRPC) and `--snapshot-path` (compiled-file fallback) sources alongside
  phase 2's `--resources`. Helm: operator `Service`, probes, `POD_NAME`,
  tightened operator `ClusterRole`, router RBAC removed (the router never
  calls the Kubernetes API). ADR-0014.
- Phase 2 inline ingress: `routed serve --mode inline` (axum/hyper) with a
  zero-buffering streaming forwarder, byte-preserving `model` / reasoning
  rewrites, `X-Routed-*` response headers, BLOCK as OpenAI 403, dry-run,
  `POST /v1/decide`, `POST /v1/feedback` (accepted + logged), `/healthz`,
  `/readyz`, Prometheus `/metrics`, OTLP `routed.decision` spans, file-based
  snapshot source with hot reload, `type: http` classifier, mock gateway,
  in-process integration tests (streaming byte identity, gated release, cancel
  propagation, idle watchdog, security table) and a kind e2e (`make e2e`).
  Helm: probes, resources ConfigMap, OTLP endpoint. ADRs 0012-0013.
- Phase 1 core engine: `routed.io/v1alpha1` CRD types with generated manifests
  (`routedctl crd gen`), the policy compiler producing content-hashed snapshots,
  the deterministic decision engine (hard constraints before scoring, four
  objective modes, fallback semantics), heuristic classifier with regex/checksum
  PII and injection heuristics, restriction-only header hints, `routedctl
  validate` and `routedctl explain`, 23 golden examples, property tests, and
  the engine latency gate. ADRs 0006-0011.
- Phase 0 scaffold: Cargo workspace (one crate per architectural seam), containerized
  Rust toolchain, Makefile/CI mirror, governance files, ADRs 0001-0005, Helm chart
  skeleton, empty-but-compiling `routed`, `routed-operator`, and `routedctl` binaries.
