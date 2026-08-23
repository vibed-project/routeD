# routeD

**A Kubernetes-native semantic router for AI models.** routeD is the *brain*
that decides which model should serve a request; your existing LLM gateway is
the *hands* that enforces the decision and calls the provider.

```
Agent / User  ->  routeD (decide)  ->  Gateway (enforce, call provider)  ->  Model
```

**Documentation: https://vibed-project.github.io/routeD/**

routeD optimises primarily for **cost** and **security / data sovereignty**,
secondarily for quality and latency, and explains every decision in a
machine-readable, auditable form.

> Status: **v0.1.0**. Both ingress modes work end to end, the operator
> reconciles CRDs into distributed snapshots with admission validation,
> ONNX classifiers and the learned router load behind the `onnx` feature,
> the feedback loop feeds the offline trainer, and releases ship signed
> images with SBOMs (`docs/threat-model.md`, ADR-0019).

## What routeD is, and is not

routeD **is** a decision layer:

- classifies each request locally (task type, complexity, sensitivity / PII,
  injection risk) with small ONNX models and cheap heuristics,
- applies hard constraints first (data class, jurisdiction, CLOUD Act
  exposure, operator control, capabilities, risk caps, cost caps),
- scores the surviving candidate tiers for cost, quality and latency,
- rewrites the OpenAI-compatible `model` field and sets `X-Routed-*` headers,
- emits a `routed.decision` OpenTelemetry span with the full explanation.

routeD **is not** a gateway. It never holds provider API keys, never calls
model providers, and does not implement provider adapters, retries, rate
limiting or billing. It works with LiteLLM, Envoy AI Gateway,
agentgateway / kgateway, Kong AI Gateway and any other OpenAI-compatible gateway.

| Responsibility | routeD | Gateway |
|----------------|:------:|:-------:|
| Classify request, resolve data class | yes | |
| Choose model tier under security and cost constraints | yes | |
| Rewrite `model`, set reasoning / token parameters | yes | |
| Explain and audit the decision | yes | |
| Hold provider credentials, call providers | | yes |
| Retries, failover, rate limits, quotas, billing | | yes |
| Authenticate callers | | yes |
| Proxy and stream responses | inline mode only, unmodified | yes |

## How it works

Configuration is declarative through four CRDs in the `routed.io/v1alpha1`
group, reconciled by the operator into an immutable, hashed **snapshot** that
router pods hot-swap:

| Kind | Purpose |
|------|---------|
| `ModelTier` | A model as the gateway exposes it, with cost, quality, latency, sovereignty and security metadata |
| `DataClass` | Sensitivity levels, how they are detected, and the tiers they allow |
| `RoutingPolicy` | Scope, candidates, hard constraints, objective and fallback for a tenant / agent / path |
| `RouterProfile` | Classifier, embedder and learned-router artifacts and calibration |

Every decision is one JSON document, identical in `routedctl explain`, the
`X-Routed-Decision` header and the OpenTelemetry span:

```json
{
  "id": "01J...",
  "outcome": "ROUTE",
  "policy": "ai-platform/default-cost-secure",
  "requestedModel": "auto",
  "selectedTier": "eu-sovereign-large",
  "gatewayModel": "mistral-large-eu",
  "parameters": { "reasoning": "low" },
  "dataClass": "personal",
  "classification": { "task": "summarization", "complexity": "medium", "riskScore": 0.12, "piiEntities": ["EMAIL"] },
  "candidates": [
    { "tier": "us-cheap-small", "eliminatedBy": "dataClass.forbidCloudActExposed" },
    { "tier": "eu-sovereign-small", "predictedQuality": 0.71, "eliminatedBy": "qualityFloor" },
    { "tier": "eu-sovereign-large", "predictedQuality": 0.84, "estimatedCostEUR": 0.0031, "score": 0.91, "selected": true }
  ],
  "estimatedCostEUR": 0.0031,
  "estimatedSavingsEUR": 0.0124,
  "latencyMs": 11,
  "snapshotHash": "sha256:..."
}
```

Ingress modes (one binary, `routed serve --mode ...`):

- **`extproc`**: Envoy external processor (gRPC). Mutates the request, returns
  an immediate 403 for `BLOCK`. Primary production mode.
- **`inline`**: OpenAI-compatible HTTP forwarder to one gateway, streaming
  responses back unmodified. Convenient for development and small installs.
- **`POST /v1/decide`**: returns the decision JSON for gateways that call
  rather than proxy.

## Quickstart

**Offline** (no cluster): explore decisions with the CLI.

```sh
make build                                  # cargo inside a pinned container; no host toolchain needed
scripts/cargo-in-podman.sh cargo run -q -p routedctl -- validate config/samples examples/001-route-cost-first-basic/resources.yaml
scripts/cargo-in-podman.sh cargo run -q -p routedctl -- explain --dir examples/004-route-personal-header-eu-only
```

**Inline router in front of a mock gateway** (single container, no cluster):

```sh
scripts/cargo-in-podman.sh sh -c '
  cargo run -q -p routed-mockgateway &
  cargo run -q -p routed -- serve --mode inline --http-addr 127.0.0.1:8080 \
    --upstream http://127.0.0.1:4000 --resources examples/001-route-cost-first-basic/resources.yaml &
  sleep 3
  curl -s -D - -H "content-type: application/json" -H "X-Routed-Data-Class: personal" \
    -d "{\"model\":\"auto\",\"messages\":[{\"role\":\"user\",\"content\":\"Summarize this report.\"}]}" \
    http://127.0.0.1:8080/v1/chat/completions'
```

The response carries `X-Routed-Outcome: ROUTE`, `X-Routed-Tier: eu-sovereign-large`
and a base64 `X-Routed-Decision` explanation; the mock gateway received
`"model":"mistral-large-eu"`.

**kind** (five minutes): `make e2e` creates a `routed-e2e` cluster, installs the
Helm chart with `examples/001` resources as a ConfigMap, and asserts routing,
pass-through, BLOCK, EU-only selection, dry-run, streaming, the decision API and
metrics (`test/e2e/run.sh`). With your own gateway:

```sh
helm install routed charts/routed \
  --set mode=inline --set upstream=http://litellm:4000 \
  --set routing.enabled=true --set-file routing.files.routing\.yaml=my-routing.yaml
```

## Repository layout

```
cmd/            routed (router), routed-operator, routedctl
crates/         one crate per architectural seam (decision, policy, classify, ingress-*, ...)
config/         CRD manifests, RBAC, samples
charts/routed/  Helm chart
trainer/        Python (uv): offline router training and ONNX export
docs/           architecture, decision API, CRDs, integrations, ADRs
examples/       sample resources and golden decision fixtures
test/e2e/       kind-based end-to-end tests
```

## Roadmap

| Phase | Scope | Status |
|-------|-------|--------|
| 0 | Scaffold, toolchain, CI, governance, ADRs, chart skeleton | done |
| 1 | CRD types, policy compiler, decision engine, `routedctl validate/explain`, golden + property tests | done |
| 2 | Inline ingress with streaming, decision API, telemetry, mock gateway, kind e2e | done |
| 3 | Operator, snapshot distribution, webhooks, complete chart | done |
| 4 | ONNX classifiers, PII / injection detection, artifact fetching, benchmarks | done |
| 5 | Envoy ext_proc and gateway integration guides | done |
| 6 | Feedback loop, learned router, trainer, `routedctl simulate` | done |
| 7 | Threat model, supply chain, v0.1.0 | done |

## Contributing and governance

See `CONTRIBUTING.md` (DCO sign-off, conventional commits, `make ci`),
`GOVERNANCE.md`, `CODE_OF_CONDUCT.md` and `SECURITY.md`. Architecture decisions
live in `docs/adr/`.

## License

Apache License 2.0. See `LICENSE` and `NOTICE`.
