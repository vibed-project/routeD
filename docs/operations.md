---
sidebar_position: 4
title: Operations
---

# Operations

Running routeD in production: what gets deployed, how configuration reaches the
routers, what to watch, and how to verify what you are running.

The security model behind these mechanics, including what a compromised
component can and cannot do, is in the [threat model](./threat-model.md).

## Deployment shape

The Helm chart installs one or two Deployments plus the CRDs.

| Component | Purpose | Scaling |
|-----------|---------|---------|
| `routed` (router) | Serves the chosen ingress mode, the decision and feedback APIs, health and metrics. Holds the snapshot in an atomic pointer and hot-swaps it. | Stateless, scale horizontally |
| `routed-operator` | Watches the four CRDs, compiles them into a snapshot, serves it to routers, writes status, optionally serves the admission webhook. | Every replica compiles and serves independently; writes are leader-gated |

Ports and endpoints:

| Component | Port | Serves |
|-----------|------|--------|
| Router | 8080 (`--http-addr`) | Inline ingress (inline mode), `POST /v1/decide`, `POST /v1/feedback`, `/healthz`, `/readyz`, `/metrics` |
| Router | 9002 (`--extproc-addr`) | Envoy `ext_proc` gRPC service, ext_proc mode only |
| Operator | 8080 | Prometheus metrics |
| Operator | 8081 | Liveness and readiness probes |
| Operator | 9090 | `SnapshotService` gRPC stream |
| Operator | 9443 | Validating admission webhook, when enabled |

The chart's defaults are conservative: `runAsNonRoot` as uid 65532, a read-only
root filesystem, all capabilities dropped, the `RuntimeDefault` seccomp
profile, a liveness probe on `/healthz` and a readiness probe on `/readyz`, and
a 75 second termination grace period so in-flight streams can finish.

The router never talks to the Kubernetes API in any distribution mode, so the
chart creates **no** RBAC for it. The operator's `ClusterRole` covers only what
it needs: read the four `routed.io` kinds, update their status subresources,
and manage the fallback snapshot ConfigMap and the leader-election Lease.

### Install

```sh
helm install routed oci://ghcr.io/vibed-project/charts/routed --version 0.1.2 \
  --namespace ai-platform --create-namespace \
  --set mode=extproc \
  --set image.tag=v0.1.2 \
  --set operator.enabled=true \
  --set operator.webhook.enabled=true
```

Helm installs the CRDs on first install only. Upgrade them explicitly with
`kubectl apply -f config/crd/` before upgrading the chart.

Values worth knowing:

| Value | Default | Meaning |
|-------|---------|---------|
| `mode` | `inline` | `inline` or `extproc` |
| `upstream` | `""` | Gateway URL, inline mode only |
| `operator.enabled` | `false` | Run the operator and take snapshots from it |
| `operator.distribution` | `grpc` | `grpc` stream, or `configmap` file mount |
| `operator.watchNamespace` | `""` (all) | Restrict the operator to one namespace |
| `operator.leaderElect` | `true` | Required for more than one operator replica |
| `operator.webhook.enabled` | `false` | Serve the validating admission webhook |
| `operator.webhook.failurePolicy` | `Ignore` | `Fail` for hard admission gating |
| `routing.enabled` | `false` | Operator-less mode: mount routing YAML as a ConfigMap |
| `feedback.enabled` | `false` | Persist the decision journal and feedback for offline training |
| `otlpEndpoint` | `""` | OTLP gRPC endpoint for trace export |
| `extraContainers`, `extraVolumes` | `[]` | Sidecars and volumes appended to the router pod |

## Configuration flow

```text
CRDs  ->  operator: relist all four kinds, compile once  ->  content-hashed Snapshot
                                                                      |
                              gRPC SnapshotService.Watch (primary)  ---+---> router (hot swap)
                              ConfigMap written by the leader       ---+---> router (file mount)
```

Every operator replica watches the four kinds, relists on any change and runs
the same pure compiler that `routedctl validate` and the router's file source
run. Compilation is deterministic, so replicas independently produce
byte-identical snapshots and can serve watchers with no coordination between
them. Adding a replica improves read availability immediately.

Leader election gates only the two write paths: status on the custom resources
(`status.conditions`, plus `compiledHash` on `RoutingPolicy`) and the fallback
ConfigMap. It does not gate gRPC serving, so losing the lease never interrupts
snapshot delivery. Design and rationale:
[ADR-0014](./adr/0014-operator-reconciliation-and-distribution.md).

The router picks its snapshot source in a fixed precedence:

| Flag | Source |
|------|--------|
| `--snapshot-addr` | Operator gRPC stream (primary) |
| `--snapshot-path` | Compiled snapshot JSON file, the operator's ConfigMap fallback mounted as a volume |
| `--resources` | Local resource files compiled by the router itself, for standalone or operator-less installs |

If none is set the router never becomes ready. Both distribution paths carry
the same compiled document, so they cannot disagree with each other or with
`routedctl explain` on the same objects.

### Watching a rollout

- `kubectl get routingpolicy -n ai-platform` prints a `Ready` column for each
  policy, alongside its priority and objective mode.
- `kubectl get routingpolicy <name> -n ai-platform -o jsonpath='{.status.compiledHash}'`
  gives the snapshot hash that policy last compiled into.
- The router logs `snapshot loaded` with the hash on every swap.
- Every decision carries `snapshotHash`, and `routed_snapshot_age_seconds`
  tracks how stale the loaded snapshot is.

Matching hashes across `routedctl validate`, the policy status and a live
decision is the quickest way to confirm that what you reviewed is what is
serving.

### Trying a change before it ships

`routedctl simulate` replays a JSONL file of requests against a set of
resources offline, using the same pipeline as `routedctl explain`, and prints
outcome, tier, cost and block summaries:

```sh
routedctl simulate --policy resources.yaml --requests traffic.jsonl
```

Each line is either a bare OpenAI-format request or
`{"request": ..., "path": ..., "headers": {...}}`. No cluster is involved, so a
policy change can be evaluated against real traffic shapes before it is
applied.

### Mutual TLS for the snapshot stream

Plain TCP is the default, on the assumption that a NetworkPolicy fences the
operator's port to router pods. Where the network is not a boundary, or the
snapshot source is remote, enable mutual TLS symmetrically:

```sh
routed-operator --snapshot-tls-dir /etc/routed/tls          # requires CA-signed client certs
routed          --snapshot-tls-dir /etc/routed/tls \
                --snapshot-tls-domain routed-operator.ai-platform.svc
```

Each directory holds `tls.crt`, `tls.key` and `ca.crt`, which is a Kubernetes
TLS secret plus the peer CA, so cert-manager output mounts directly.
Certificates are read at startup only, so rotation means restarting the pods.
There is no self-signed fallback: no directory means no TLS.
See [ADR-0021](./adr/0021-snapshot-distribution-mtls.md).

## Admission validation

With `operator.webhook.enabled=true`, the operator serves `POST /validate` over
TLS and validates `CREATE` and `UPDATE` of the four kinds by running the same
compiler as the reconciler over current cluster state with the incoming object
substituted. A write is denied only by error diagnostics attributed to the
incoming object, so a pre-existing error elsewhere never blocks an unrelated
edit; warnings attributed to the object come back as admission warnings.
Deletions are allowed unexamined.

The webhook never mutates objects, which keeps `sideEffects: None` honest.

`failurePolicy` defaults to `Ignore`, deliberately: the operator's status
conditions are the authoritative asynchronous safety net, and a webhook outage
must not lock anyone out of editing the very resources that would fix it. Set
`operator.webhook.failurePolicy=Fail` for hard gating, accepting that a webhook
outage becomes a CRD write outage.

The chart generates a self-signed certificate on first install, stores it in a
`kubernetes.io/tls` Secret and reuses it on upgrade so the `caBundle` and the
served certificate cannot drift apart. Rotation is manual: delete the Secret
and upgrade, or replace it with cert-manager-managed material, since the
operator only reads files. Details in
[ADR-0015](./adr/0015-admission-validation.md).

## Telemetry

### Metrics

Prometheus text format on `/metrics`, with no request-derived labels, so
cardinality is bounded by configuration.

| Metric | Labels | Meaning |
|--------|--------|---------|
| `routed_decisions_total` | `outcome`, `policy`, `tier`, `data_class`, `mode` | Decisions taken |
| `routed_decision_latency_seconds` | histogram | Decision latency |
| `routed_estimated_cost_eur_total` | `tier` | Estimated spend routed to each tier |
| `routed_estimated_savings_eur_total` | | Estimated savings against the most expensive surviving candidate |
| `routed_classifier_latency_seconds` | `classifier` | Classification latency |
| `routed_classifier_errors_total` | `classifier`, `kind` | Classifier errors and timeouts |
| `routed_blocked_total` | `reason` | Blocks by reason |
| `routed_snapshot_age_seconds` | | Age of the loaded snapshot |
| `routed_upstream_requests_total` | `status` | Inline-mode forwarding results |
| `routed_hint_ignored_total` | `kind` | Ignored or rejected `X-Routed-*` hints |

Counter families appear in a scrape once they have their first observation.
`examples/observability/grafana-dashboard.json` is an importable dashboard over
these series.

Two of them are worth alerting on: a rising `routed_classifier_errors_total`
means decisions are falling back rather than being reasoned, and a rising
`routed_snapshot_age_seconds` means configuration changes are not reaching the
routers.

### Traces and logs

Setting `--otlp-endpoint` or `OTEL_EXPORTER_OTLP_ENDPOINT` exports a
`routed.decision` span per decision over OTLP gRPC, carrying every decision
field including the compact candidate list. Inbound W3C `traceparent` is
honoured and propagated upstream. Logs are JSON, one line per decision at
`info` with the same fields, and `RUST_LOG` controls verbosity.

Prompts are never logged or attached to spans. `--log-prompt-hashes` with
`ROUTED_PROMPT_HASH_SALT` records a salted SHA-256 of the classified text for
correlation, and is off by default. Schema:
[ADR-0013](./adr/0013-telemetry-schema.md).

Latency budgets and how to run the gates are in
[performance](./performance.md).

## The feedback loop

Feedback is optional, local and offline. It never changes routing while the
router is running.

```text
router --feedback-dir  ->  decisions.jsonl + feedback.jsonl  ->  trainer  ->  model.onnx
                                                                                  |
                                                                                  v
                                        RouterProfile.spec.learnedRouter.uri (digest-pinned)
```

1. **Collect.** `--feedback-dir` (Helm: `feedback.enabled=true`, default
   `/var/lib/routed/feedback` on an emptyDir) appends two JSONL streams through
   a bounded channel and a writer task. The request path never blocks on disk;
   when the channel is full, records are dropped and counted.
   - `decisions.jsonl`: one record per decision. Closed-vocabulary labels and
     routing facts only, never request content.
   - `feedback.jsonl`: one record per accepted `POST /v1/feedback`, joined to
     decisions on `decisionId`.
2. **Ship.** The files grow unbounded on an emptyDir. Rotate them or run a
   sidecar, which is what `extraContainers` and `extraVolumes` are for.
3. **Train.** Export the snapshot the decisions were made against, then run the
   offline trainer from `trainer/` (Python 3.12 with `uv`, numpy only):

   ```sh
   routedctl validate resources.yaml --emit-snapshot snapshot.json
   uv run routed-train \
     --decisions feedback/decisions.jsonl \
     --feedback feedback/feedback.jsonl \
     --snapshot snapshot.json \
     --out out/
   ```

4. **Deploy.** Hash and publish `out/model.onnx`, set
   `RouterProfile.spec.learnedRouter.uri` to the digest-pinned URI, and enable
   it per policy with `learnedRouter.enabled` and `minConfidence`. A router
   built with the `onnx` feature loads it at startup.

A trained model can only refine `predictedQuality`. Poisoned feedback can
therefore degrade a future, offline-trained model, and that model still cannot
loosen a hard constraint. Feedback is not authenticated beyond your platform's
ingress controls. Contract:
[ADR-0018](./adr/0018-feedback-records-and-learned-router.md).

## Model artifacts

Classifier, embedder and learned-router artifacts are referenced by URI from
`RouterProfile` and resolved into a content-addressed cache:

| Scheme | Trust |
|--------|-------|
| `https://...@sha256:<hex>` | Digest pin mandatory; fetched bytes must match |
| `oci://<registry>/<repo>@sha256:<manifest digest>` | Manifest verified against the pin, then the layer against the manifest. Tags are rejected |
| `file:///...` | Loaded as-is; an optional `@sha256:` suffix is verified when present |

Cache hits re-verify the digest, downloads are written to a temp file and
renamed into place only after verification, and a mismatch never enters the
cache. `routedctl models pull <uri>` pre-warms the same cache the router uses.
The cache directory comes from `ROUTED_MODEL_CACHE`, defaulting to
`~/.cache/routed/models`. Because the chart runs the router with a read-only
root filesystem, an install that fetches remote artifacts needs a writable
volume with `ROUTED_MODEL_CACHE` pointing at it.

Set `ROUTED_ARTIFACT_COSIGN_PUB` to a cosign public key to additionally require
a valid signature on every `oci://` artifact, checked before any artifact bytes
are fetched. It fails closed in both directions: a missing signature manifest
is a verification failure rather than a fallback to pin-only trust, and an
unreadable key makes every `oci://` fetch fail rather than silently skipping
verification. Scope is `oci://` only, since `https://` and `file://` have no
standardised signature channel. See
[ADR-0022](./adr/0022-cosign-artifact-verification.md) and
[ADR-0016](./adr/0016-artifact-resolution-and-onnx-contract.md).

## Supply chain

Releases are built only in CI, from a tag, by
`.github/workflows/release.yml`.

| Artifact | Where | Verification |
|----------|-------|--------------|
| `routed`, `routed-operator` images | `ghcr.io/vibed-project/*`, multi-arch | cosign keyless signatures using the GitHub OIDC workflow identity |
| Helm chart | `oci://ghcr.io/vibed-project/charts/routed` | Chart version equals the release version |
| SBOMs | Attached to the GitHub release | CycloneDX, one per binary, generated by `cargo cyclonedx` (`make sbom`) |
| Release files | GitHub release | `SHA256SUMS` |

Verify an image before rolling it out:

```sh
cosign verify \
  --certificate-identity-regexp '^https://github.com/vibed-project/routed/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/vibed-project/routed:v0.1.2
```

`cargo deny` gates advisories and licenses in CI, and the toolchain and runtime
images are digest-pinned Debian releases kept in lockstep. Local builds are
marked `-dev` and are never signed. Release process and rationale:
[ADR-0019](./adr/0019-supply-chain-and-release.md).

Signing authenticates *what* you are running. It does not authenticate
*choices*: whoever can write a `RouterProfile` chooses which model is pinned,
and whoever can write a `RoutingPolicy` controls routing. That is the intended
administrative boundary, and Kubernetes RBAC is what protects it.

## Access to the decision APIs

`POST /v1/decide` and `POST /v1/feedback` are cluster-internal APIs. By default
they are unauthenticated, on the assumption that a NetworkPolicy or service
mesh fences them. For deployments that need caller identity, the inline ingress
crate exposes an `Authenticator` seam whose default is allow-all; an
implementation is composed in at build time and enforced on both endpoints
before any body is read, returning the standard OpenAI error envelope on
denial. `/healthz`, `/readyz` and `/metrics` stay open for probes and scrapers.

End-user traffic is not authenticated here by design: those credentials belong
to the gateway behind the router, as do rate limiting and quotas. See
[ADR-0020](./adr/0020-decision-api-authentication-seam.md) and
[ADR-0001](./adr/0001-decision-layer-not-gateway.md).

## Operational checklist

| Check | Why |
|-------|-----|
| The gateway is configured to fail closed | `failure_mode_allow: false` for ext_proc, and a hook that raises rather than falling through for decision-API integrations. Otherwise routeD being unreachable silently disables routing |
| The gateway honours the rewritten `model` | It is the enforcement point; a gateway that ignores the rewrite defeats routing entirely |
| A NetworkPolicy fences the operator's gRPC port, or mTLS is on | The snapshot stream is plain TCP by default |
| The feedback journal is rotated or shipped | It grows unbounded on an emptyDir |
| Snapshot hashes match across validate, status and live decisions | Confirms the reviewed configuration is the serving one |
| CRD write access is treated as production access | Anyone with it controls routing |

## Related reading

- [Threat model](./threat-model.md): trust boundaries, residual risks and
  tracked gaps.
- [Architecture](./architecture.md): component and distribution diagrams.
- [Gateway integrations](./integration/README.md): required gateway
  configuration per product.
- [Performance](./performance.md): latency budgets and how they are gated.
