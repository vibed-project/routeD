# Gateway integrations

routeD hands every decision to an existing gateway. Three integration shapes
exist:

- **Inline** (`routed serve --mode inline --upstream <gateway>`): put routeD in
  front of the gateway's OpenAI-compatible endpoint. Works with any gateway,
  including LiteLLM, Kong AI Gateway and Envoy AI Gateway, with no gateway
  configuration beyond pointing clients at routeD.
- **ext_proc** (`routed serve --mode extproc`, ADR-0017): for Envoy-based
  gateways, routeD joins the filter chain as an external processor and
  mutates the request in place. No extra hop; response streaming never
  passes through routeD.
- **Decision API** (`POST /v1/decide`): the gateway calls routeD with the raw
  request and headers and applies the returned `gatewayModel` / `parameters`
  itself (for example from a LiteLLM `pre_call` hook). The request path can be
  passed as `X-Routed-Path` when it is not `/v1/chat/completions`.

Per-gateway guides:

- [`litellm.md`](litellm.md): inline mode, or a pre-call hook on `POST /v1/decide`
- [`envoy-ai-gateway.md`](envoy-ai-gateway.md): the ext_proc filter configuration
- [`agentgateway.md`](agentgateway.md): kgateway / agentgateway extension
- [`kong.md`](kong.md): inline mode, or a Kong plugin on `POST /v1/decide`

Required gateway behaviour in every case: honour the rewritten `model` field
and the `X-Routed-*` headers, and fail closed (`failure_mode_allow: false`)
when routeD is unreachable.
