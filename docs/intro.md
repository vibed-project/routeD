---
slug: /
sidebar_position: 1
title: Introduction
---

# routeD

routeD is a Kubernetes-native semantic router for AI models. It is the layer
that decides **which model should serve a request**. Your existing LLM gateway
enforces that decision and calls the provider.

```text
Agent / User  ->  routeD (decide)  ->  Gateway (enforce, call provider)  ->  Model
```

routeD optimises primarily for **cost** and for **security and data
sovereignty**, secondarily for quality and latency, and explains every decision
in a machine-readable, auditable form.

## The decision pipeline

For each request routeD:

1. **Classifies it locally**, with cheap heuristics and optional small ONNX
   models: task type, complexity, sensitivity and PII, injection risk.
2. **Applies hard constraints first**: data class, jurisdiction, CLOUD Act
   exposure, operator control, capabilities and context window, risk caps and
   cost caps. Constraints eliminate candidates; they are never weights.
3. **Scores the surviving candidates** for cost, quality and latency according
   to the policy objective.
4. **Rewrites the OpenAI-compatible `model` field**, injects reasoning and
   token parameters, and sets `X-Routed-*` response headers.
5. **Emits a `routed.decision` OpenTelemetry span** carrying the full
   explanation, including every candidate that was eliminated and why.

Steps 2 and 3 are in a fixed, non-configurable order: no flag, environment
variable or request header can move a cost decision ahead of a security
constraint. See [ADR-0003](./adr/0003-security-before-cost.md).

## What routeD is not

routeD is not a gateway. It never holds provider API keys, never calls model
providers, and does not implement provider adapters, retries, rate limiting,
quotas or billing. It works with LiteLLM, Envoy AI Gateway, agentgateway and
kgateway, Kong AI Gateway, and any other OpenAI-compatible gateway.

| Responsibility | routeD | Gateway |
|----------------|:------:|:-------:|
| Classify the request, resolve the data class | yes | |
| Choose the model tier under security and cost constraints | yes | |
| Rewrite `model`, set reasoning and token parameters | yes | |
| Explain and audit the decision | yes | |
| Hold provider credentials, call providers | | yes |
| Retries, failover, rate limits, quotas, billing | | yes |
| Authenticate end-user callers | | yes |
| Proxy and stream responses | inline mode only, unmodified | yes |

Because routeD only decides, the blast radius of a fully compromised router is
misrouting rather than credential loss. That reasoning, and its limits, is
written up in the [threat model](./threat-model.md) and in
[ADR-0001](./adr/0001-decision-layer-not-gateway.md).

## Every decision is one auditable document

A decision is a single JSON document, byte-identical in three places: the
`routedctl explain` output, the `X-Routed-Decision` response header, and the
`routed.decision` span. It names the matched policy, the effective data class,
the classification, every candidate with its score or its elimination reason,
the estimated cost and savings, and the hash of the configuration snapshot the
decision was made against.

Elimination reasons come from a closed vocabulary, so audits can be written
against them. Prompts are never logged or persisted. The full field reference
is in the [decision API](./decision-api.md).

## Declarative configuration

Routing is configured with four namespaced custom resources in the
`routed.io/v1alpha1` API group, which the operator compiles into one immutable,
content-hashed snapshot that router pods hot-swap.

| Kind | Purpose |
|------|---------|
| `ModelTier` | A model as the gateway exposes it, with cost, quality, latency, sovereignty and security metadata |
| `DataClass` | Sensitivity levels, how they are detected, and the tiers they allow |
| `RoutingPolicy` | Scope, candidates, hard constraints, objective and fallback for a tenant, agent or path |
| `RouterProfile` | Classifier, embedder and learned-router artifacts, plus the cost model |

Field-by-field reference: [CRDs](./crds.md).

## Three ways to integrate

One binary covers all three: `routed serve --mode extproc` or `--mode inline`
chooses the ingress, and `POST /v1/decide` is served in both modes.

| Shape | How it works |
|-------|--------------|
| `extproc` | Envoy external processor over gRPC. Mutates the request in place, answers `BLOCK` with an immediate 403. No extra network hop; response bodies never pass through routeD. Primary production mode. |
| `inline` | OpenAI-compatible HTTP forwarder in front of one gateway, streaming responses back unmodified. Convenient for development and small installs. |
| `POST /v1/decide` | Returns the decision JSON for gateways that prefer to call out rather than be proxied, for example from a LiteLLM pre-call hook. |

Per-gateway guides live under [integrations](./integration/README.md).

## Status

routeD is at **v0.1.2** and pre-1.0, so minor versions may still break APIs.
Both ingress modes work end to end, the operator reconciles CRDs into
distributed snapshots with admission validation, ONNX classifiers and the
learned router load behind the `onnx` cargo feature, the feedback loop feeds
the offline trainer, and releases ship cosign-signed images with CycloneDX
SBOMs. The delivered scope per phase is in the [roadmap](./roadmap.md).

The project is licensed under Apache-2.0.

## Where to go next

- [Quickstart](./quickstart.md): get a routing decision out of routeD in a few
  minutes, with or without a cluster.
- [How routing works](./how-routing-works.md): the decision pipeline in depth,
  for deciding whether to trust it.
- [Operations](./operations.md): running routeD in production.
- [Architecture](./architecture.md): component and snapshot-distribution
  diagrams.
- [Architecture decision records](./adr/README.md): the contracts behind all of
  the above.
