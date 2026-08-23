# agentgateway / kgateway

kgateway (and agentgateway as its AI data plane) is Envoy-based, so routeD
integrates the same way as any Envoy: as an `ext_proc` filter (ADR-0017).

## Attach the processor

Use kgateway's filter extension to add the external processor to the
gateway's HTTP filter chain, pointing at the routeD `Service` (`grpc`,
port 9002). The required filter settings are the same as in
[`envoy-ai-gateway.md`](envoy-ai-gateway.md):

- `processing_mode`: request headers `SEND`, request body `BUFFERED`,
  response headers `SEND`, response body `NONE`
- `allow_mode_override: true`
- `failure_mode_allow: false`
- `message_timeout: 2s`

With kgateway's `GatewayExtension` / route-level `ExtensionRef` mechanism,
declare the extension once and reference it from the routes that carry
OpenAI-compatible traffic; scoping it to those routes avoids the header
round-trip for unrelated traffic entirely.

## Alternative: decision API

Where a filter extension is not available, call `POST /v1/decide` from the
gateway's request-transformation stage and apply `gatewayModel` /
`parameters` yourself, as in [`litellm.md`](litellm.md) option B. Pass the
original request path in `X-Routed-Path` when it is not
`/v1/chat/completions`.

Required behaviour in both shapes: honour the rewritten `model`, forward
the request unmodified otherwise, and fail closed when routeD is
unreachable.
