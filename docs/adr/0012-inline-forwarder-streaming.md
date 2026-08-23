# ADR-0012: Inline forwarder and streaming

## Status

Accepted

## Context

Inline mode (`routed serve --mode inline --upstream <gateway>`) is an
OpenAI-compatible HTTP hop in front of exactly one gateway. It must rewrite the
`model` field and headers without ever buffering or altering the upstream
response, including server-sent events, and it must fail closed on bodies it
cannot parse.

## Decision

- Server: `axum` 0.8 on `hyper` 1. One HTTP listener serves the proxy paths,
  `POST /v1/decide`, `POST /v1/feedback`, `/healthz`, `/readyz` and `/metrics`.
- Routed paths: `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings`,
  `/v1/messages`, `/v1/responses`. Every other path and method is forwarded
  verbatim (headers minus hop-by-hop and inbound `x-routed-*`, body streamed).
- Request side: `DefaultBodyLimit` (default 10 MiB, configurable) is enforced
  before any parsing; the body is parsed as a top-level
  `Map<String, Box<RawValue>>` so only `model` and the injected parameters are
  rewritten and every other field is forwarded byte for byte. Parse failures
  are a 400 in OpenAI error format; nothing is forwarded.
- Decision outcomes: `ROUTE` rewrites `model` (and `reasoning_effort` /
  `reasoning.effort` when the policy sets a reasoning budget); `PASS_THROUGH`
  forwards untouched; `BLOCK` returns 403 with an OpenAI error envelope
  (`code: routed_policy_blocked`) even for `stream: true`. `X-Routed-Dry-Run`
  returns the Decision JSON and never forwards.
- Response side: the upstream `hyper::body::Incoming` is returned as the axum
  body unchanged; no compression layer, no buffering, `TCP_NODELAY`, hop-by-hop
  headers dropped, `content-length` preserved. Client disconnects drop the
  upstream request; an idle watchdog (`--stream-idle-timeout`, default 60 s)
  ends stalled streams.
- Upstream client: `hyper-util` legacy client with `hyper-rustls` (HTTP and
  HTTPS, HTTP/1.1 and HTTP/2 over TLS), connection pooling, no automatic
  decompression.
- Response headers: `X-Routed-Decision-Id`, `X-Routed-Tier`,
  `X-Routed-Data-Class`, `X-Routed-Outcome`, `X-Routed-Estimated-Cost` and
  `X-Routed-Decision` (base64 JSON; when the full document exceeds 4 KiB a
  compact subset is sent and the full document lives on the span).
- Classifiers run on `tokio::task::spawn_blocking` behind a semaphore with the
  profile timeout; timeouts and errors become degraded findings (ADR-0006).

## Consequences

- Streaming integrity is proven by byte-identity and gated-release tests, not
  timing (a dedicated test-pyramid ADR may follow).
- Inline mode terminates the client connection and therefore needs TLS in
  front (Ingress / mesh); ext_proc mode (phase 5) avoids the extra hop.

## Alternatives considered

- `reqwest` for the upstream: rejected (automatic decompression and larger
  dependency surface); `hyper-util` gives direct control over the body.
- Typed request structs: rejected; unknown vendor fields would be dropped.
