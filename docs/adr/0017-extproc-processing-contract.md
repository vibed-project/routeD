# ADR-0017: ext_proc processing contract

## Status

Accepted

## Context

Inline mode (ADR-0012) terminates the client connection and adds an HTTP
hop. For Envoy-based gateways (Envoy AI Gateway, agentgateway / kgateway,
Istio) routeD can instead join the filter chain as an
`envoy.service.ext_proc.v3.ExternalProcessor` and mutate the request in
place: no extra hop, no connection termination, and response streaming never
passes through routeD at all. Both ingress modes must agree on every
decision semantic (ADR-0007 header trust, the BLOCK envelope, dry-run, the
`X-Routed-*` response headers), so the implementation must share the inline
pipeline rather than re-implement it.

## Decision

### One pipeline, two ingresses

`crates/ingress-extproc` reuses `routed-ingress-inline`'s decision pipeline
(`decide_bytes`, `rewrite_body`, the BLOCK envelope, the `X-Routed-*` header
writer) over the shared `AppState`. In `--mode extproc` the binary serves
the ext_proc gRPC service on `--extproc-addr` and the decision / feedback /
health / metrics APIs on `--http-addr` (the same axum routes minus the
proxy fallback); the upstream client exists but is never contacted.
Decisions are recorded with mode label `extproc`.

### Required filter configuration

- `processing_mode`: request headers `SEND`, request body `BUFFERED`,
  response headers `SEND`, response body / trailers `NONE`.
- `allow_mode_override: true`: for non-routed requests (wrong method or
  path) the server answers the header phase with a `mode_override` that
  skips the body and response phases, so pass-through traffic costs one
  header exchange.
- `failure_mode_allow: false`: fail closed, consistent with the inline
  mode's readiness behaviour (`docs/integration/`).
- `message_timeout` of at least one second: the body-phase response
  includes classification.

### Per-phase behaviour

One gRPC stream corresponds to one HTTP request; the stream is the unit of
state (buffered body, made decision).

- Request headers: non-routed -> strip inbound `x-routed-*` (ADR-0007) and
  skip the rest. Routed -> continue, expect the body. A routed request with
  `end_of_stream` (no body) is decided over empty bytes and gets the same
  400 the inline mode produces.
- Request body (buffered; chunks tolerated and accumulated up to
  `max_body_bytes`, over -> immediate 413): run the pipeline.
  - `BLOCK` -> `ImmediateResponse` 403 with the shared OpenAI envelope.
  - Dry-run -> `ImmediateResponse` 200 with the decision JSON.
  - `ROUTE` -> body mutation with the rewritten JSON (model + injected
    parameters, every other byte preserved) plus removal of inbound
    `x-routed-*`.
  - `PASS_THROUGH` -> header removal only.
- Response headers: the stored decision becomes `X-Routed-*` response
  headers (overwrite semantics), so callers see exactly what inline mode
  sends. Response bodies are never sent to routeD; SSE / streaming
  integrity is Envoy's concern in this mode.

## Consequences

- Behaviour parity between modes is structural (same functions), not
  aspirational; the protocol tests drive the generated Envoy client against
  the real tonic server.
- Envoy buffers routed request bodies (as inline mode does); response
  streaming is zero-copy through Envoy.
- The generated protos come from the `envoy-types` crate (tonic 0.14
  bindings); no vendored proto tree to maintain.

## Alternatives considered

- Vendoring the envoy proto closure and compiling with protox: kept as the
  fallback if `envoy-types` ever lags the workspace's tonic; not worth the
  maintenance while the crate tracks tonic promptly.
- Setting `X-Routed-*` on the mutated upstream request: rejected; inline
  mode deliberately strips routed headers toward the gateway (ADR-0007) and
  parity wins. Gateways that need the decision use `POST /v1/decide`.
- `FULL_DUPLEX_STREAMED` body mode: unnecessary; routed requests are small
  JSON bodies and BUFFERED matches inline's read-then-decide semantics.
