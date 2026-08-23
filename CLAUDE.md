# CLAUDE.md — working notes for routeD

Read `docs/architecture.md` and `docs/adr/` before changing contracts.

## What this repo is

routeD is a Kubernetes-native **decision layer** for AI model routing. It
decides which model tier serves a request (security / sovereignty first, then
cost and quality) and hands enforcement to an existing LLM gateway. It is not a
gateway (ADR-0001).

### Vocabulary (normative)

| Term | Meaning |
|------|---------|
| Decision | The routing verdict: `ROUTE`, `PASS_THROUGH` or `BLOCK`, plus explanation. One JSON shape everywhere (`docs/decision-api.md`). |
| Snapshot | Immutable, content-hashed compilation of all CRDs that a decision is made against. Hot-swapped atomically. |
| ModelTier | CRD: a model as the gateway exposes it plus cost / quality / latency / sovereignty / security metadata. routeD never calls it. |
| DataClass | CRD: a sensitivity level, its detection rules and the tier constraints it imposes. Most restrictive wins. |
| RoutingPolicy | CRD: scope match, candidates, hard constraints, objective, learned-router settings, fallback. |
| RouterProfile | CRD: classifier / embedder / learned-router artifacts and calibration. |
| Router | The `routed` binary. `serve --mode inline` or `--mode extproc`. |
| Operator | The `routed-operator` binary: reconciles CRDs into snapshots and distributes them. |
| routedctl | CLI: validate, explain, simulate, models pull. Uses the same compiler and engine as the router. |
| Gateway | The external enforcement point (LiteLLM, Envoy AI Gateway, ...). Never routeD itself. |
| Hard constraint | A rule that eliminates candidates (never a weight). Evaluated before scoring (ADR-0003). |
| Hint | An untrusted `X-Routed-*` request header. Can only make a decision more restrictive. |

## Current milestone: post-v0.1.0 maintenance

Done: phase 0 scaffold; phase 1 CRD types (`crates/api`, generated
`config/crd`), policy compiler, decision engine, heuristic classifier, security
heuristics, `routedctl validate/explain/crd`, goldens in `examples/`, property
tests, latency gate.

Done additionally: phase 2 inline ingress (`crates/ingress-inline`), telemetry
(`crates/telemetry`), mock gateway (`test/mockgateway`), kind e2e (`test/e2e`),
file-based snapshot source in `cmd/routed`.

Done additionally: phase 3 operator (`cmd/routed-operator`): watches the four
CRDs, compiles one snapshot per change, distributes it over a `SnapshotService`
gRPC stream (`crates/proto`) and a `ConfigMap` fallback, writes
`status.conditions` / `compiledHash` from compile diagnostics, and gates those
writes (not gRPC serving) behind `Lease`-based leader election. Router gained
`--snapshot-addr` / `--snapshot-path` sources. Helm chart completed for this
(operator `Service`, probes, tightened RBAC). ADR-0014. The validating
webhook landed as its fast-follow: compile-based admission with per-object
diagnostic attribution, fail-open by default, Helm-generated certificate
(ADR-0015).

Done additionally: phase 4 local inference: `crates/artifact` (digest-pinned
https/file artifact cache, `routedctl models pull`), the ONNX classifier
(feature `onnx`, `ort` load-dynamic + `tokenizers`, multi-head contract in
ADR-0016, heuristics floor the model), `classifier.tokenizerUri` in the CRD,
`make onnx` (fetches `libonnxruntime.so`, gates p95 < 30 ms under
`ROUTED_PERF=1`), classifier criterion benches, committed test fixtures
(regenerable via `trainer/scripts/make_classifier_fixture.py`). Dedicated
PII / injection detector models ride the same plumbing later (ADR-0016).

Done additionally: phase 5 ext_proc ingress (`crates/ingress-extproc`,
ADR-0017): `routed serve --mode extproc` serves the Envoy `ext_proc` v3
service on `--extproc-addr` and the decision/feedback/health/metrics APIs on
`--http-addr`, reusing the inline pipeline (`decide_bytes`, `rewrite_body`,
the BLOCK envelope) over shared `AppState`. Protos via the `envoy-types`
crate. In-process protocol tests plus a real-Envoy kind e2e scenario
(`test/e2e/run.sh test-extproc`). Gateway guides in `docs/integration/`.

Done additionally: phase 6 learning loop (ADR-0018): `crates/feedback`
(decision journal + feedback JSONL via a bounded non-blocking sink,
`--feedback-dir`, Helm `feedback.enabled`), the learned router
(`routed-features/1` layout in `crates/classify/src/router_features.rs`,
`OnnxQualityPredictor` into the engine's `QualityPredictor` seam, loaded
from `learnedRouter.uri`, per-policy `minConfidence` gating), `trainer/`
(`uv run routed-train`: join, featurise, numpy logistic fit, ONNX export +
calibration), `routedctl simulate` (JSONL replay with aggregate summaries)
and `routedctl validate --emit-snapshot`. Every routedctl subcommand is
implemented.

Done additionally: phase 7 (ADR-0019): `docs/threat-model.md`, oci://
artifact pulls (manifest-digest trust chain in `crates/artifact`),
CycloneDX SBOMs (`make sbom`, cargo-cyclonedx in the toolchain image), the
release workflow (`.github/workflows/release.yml`: multi-arch images,
cosign keyless signing, chart as OCI, checksummed GitHub release), and the
v0.1.0 cut (workspace + chart version, CHANGELOG section, goldens
regenerated for the new compiler version).

In scope now: maintenance and the tracked gaps in the threat model (mTLS
for snapshot distribution, artifact signature verification, credentialed
OCI registries).

## Invariants

- Hard constraints run before any cost / quality scoring; the order is fixed.
- `crates/decision`, `crates/policy`, `crates/snapshot`, `crates/api` are pure
  (no tokio / kube client / network / ONNX). `make boundary` enforces it.
- Untrusted headers can only tighten decisions. Inbound `X-Routed-*` headers
  are stripped before forwarding.
- Prompts are never logged or persisted.
- Determinism: same snapshot + same request + same findings = same decision.
  Use `BTreeMap`, round floats at serialisation.
- Fail safe: classifier timeout => policy `fallbackDecision`; no snapshot => not ready.

## Build and test

There is **no host Rust toolchain**. Use `make` targets; they run cargo inside
`localhost/routed-toolchain:<pin>` via `scripts/cargo-in-podman.sh` (podman).
`make ci` mirrors GitHub Actions. Caches live in `.cache/` and `target/`.

## Layout map

```
cmd/{routed,routed-operator,routedctl}   binaries (clap)
crates/version      build metadata          crates/api        CRD types (routed.io/v1alpha1)
crates/snapshot     immutable snapshot      crates/policy     compiler CRDs -> Snapshot
crates/decision     pure engine             crates/classify   classifier traits + impls
crates/embed        embeddings              crates/security   PII, injection, header hints
crates/cache        semantic cache          crates/router     pipeline glue
crates/feedback     feedback API/export     crates/telemetry  OTel, metrics, logs
crates/ingress-inline  OpenAI forwarder     crates/ingress-extproc  Envoy ext_proc
config/ charts/routed/ docs/ examples/ test/e2e/ trainer/ build/ scripts/
```

## Conventions

- SPDX header on every `.rs` / `.sh` / `.py` file.
- Conventional commits, `git commit -s` (DCO).
- clippy pedantic is on; `unwrap_used` is denied outside tests.
- Write an ADR when a contract changes (see `docs/adr/README.md`).
- Plain prose in docs; no em dashes.
