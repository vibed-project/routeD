# ADR-0001: routeD is a decision layer, not a gateway

## Status

Accepted

## Context

Organisations running many AI models already operate an LLM gateway (LiteLLM,
Envoy AI Gateway, agentgateway/kgateway, Kong AI Gateway, Portkey, Bifrost).
Those gateways hold provider credentials, implement provider adapters, retries,
rate limits, and billing. What they lack is a principled answer to *which* model
should serve a given request when cost, quality, latency, and data sovereignty
all matter. Building another gateway would duplicate mature infrastructure and
force users to migrate credentials and adapters.

## Decision

routeD is **only** a decision layer. Mental model:

```
Agent/User -> routeD (decide) -> Gateway (enforce, call provider) -> Model
```

- routeD's output is a `Decision` (`ROUTE`, `PASS_THROUGH`, or `BLOCK`) with a
  machine-readable explanation. It is expressed in a gateway-agnostic way: a
  rewritten `model` field in the OpenAI-compatible request body plus
  standardized `X-Routed-*` headers.
- routeD never holds provider API keys, never talks to model providers, and
  never implements provider adapters, provider retries, rate limiting, or
  billing.
- Three integration shapes share one decision engine: Envoy `ext_proc` (primary,
  mutate-only), an inline OpenAI-compatible forwarder (convenience, forwards to
  exactly one configured gateway), and `POST /v1/decide` for gateways that call
  rather than proxy.
- Every decision is serialisable to one JSON document (`docs/decision-api.md`)
  that is identical across `routedctl explain`, the `X-Routed-Decision` header,
  and the `routed.decision` OpenTelemetry span, so routing is explainable and
  auditable independent of the gateway.

## Consequences

- routeD composes with any OpenAI-compatible stack and can be removed without
  breaking the data plane.
- Enforcement correctness depends on the gateway honouring the rewritten
  `model` field; integration docs state the required gateway configuration.
- Features that belong to gateways (quotas, provider failover) are explicitly
  out of scope even when users ask for them.

## Alternatives considered

- **Build a full gateway.** Rejected: duplicates mature projects, forces
  credential migration, enlarges the security surface.
- **Plugin inside one specific gateway.** Rejected: ties the decision logic to
  one vendor; the `ext_proc` and `/v1/decide` contracts keep it portable.
