# ADR-0020: Decision API authentication seam

## Status

Accepted (2026-08-23)

## Context

`/v1/decide` and `/v1/feedback` are cluster-internal APIs. Many deployments
run them behind a service mesh or network policy and need no caller
authentication; others must attribute calls to a principal (a platform
team's tooling, a gateway service account) or reject unauthenticated
callers outright. The router itself should stay credential-agnostic: it
must not grow bindings to any particular identity provider, and the
decision path must not pay for machinery it does not use.

## Decision

`routed-ingress-inline` exposes a pluggable seam (`authn` module):

- `trait Authenticator`: `authenticate(&HeaderMap) -> AuthDecision`, where
  `AuthDecision` is `Allow(Identity)` or `Deny { status, reason }`
  (401/403). `Identity` carries a subject and group list.
- The default is `AllowAll` (anonymous, always allowed) — existing
  behaviour is unchanged and the default build has no new dependencies.
- Deployments plug an implementation in via
  `AppState::with_authenticator`, the same composition pattern as
  `with_predictor` and `with_feedback` (ADR-0004).

Enforcement points are `/v1/decide` and `/v1/feedback`, before any body
read or classification work. The forwarding path (`/v1/chat/completions`
et al.) is not gated here: end-user traffic authenticates at the gateway
behind the router, which owns those credentials (ADR-0001). `/healthz`,
`/readyz` and `/metrics` stay open — probes and scrapers carry no
credentials.

Rules for implementations, in line with the restriction-only philosophy
(ADR-0007): an authenticator can only deny a request or attach identity;
it cannot influence the decision, and it must never log or propagate
credential material. Denials return the standard OpenAI-format error
envelope.

## Consequences

- Zero behaviour change and zero new dependencies by default.
- Identity is established but not yet consumed by the router; feedback
  attribution or per-principal telemetry can build on the seam later
  without changing it.
- Implementations needing remote key material (JWKS and similar) must
  cache it and stay cheap per call; the trait is synchronous by design to
  keep the hot path free of await points.
