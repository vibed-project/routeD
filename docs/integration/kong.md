# Kong AI Gateway

Kong is not Envoy-based, so the `ext_proc` mode does not apply. Two shapes
work:

## Option A: inline mode in front of Kong

Point clients at routeD and routeD at Kong's OpenAI-compatible route:

```sh
routed serve --mode inline --upstream http://kong:8000 \
  --resources /etc/routed/resources
```

Kong's `ai-proxy` plugin then receives the rewritten `model`. Configure the
plugin with `route_type: llm/v1/chat` and model selection from the request
body so routeD's rewrite is honoured.

## Option B: decision API from a plugin

Call `POST /v1/decide` from a custom plugin (Lua or the WASM plugin
runtime) in the access phase:

1. Read the buffered request body and the caller's headers.
2. `POST` the body to `http://routed:8080/v1/decide` with the original path
   in `X-Routed-Path`; forward any `X-Routed-*` caller hints verbatim
   (routeD treats them as untrusted and only lets them tighten the
   decision, ADR-0007).
3. On `BLOCK`, terminate with the returned 403 envelope. On `ROUTE`, set
   `body.model = decision.gatewayModel`, merge `decision.parameters`, and
   let `ai-proxy` continue.
4. Fail closed: terminate with 503 when routeD is unreachable, matching
   `failure_mode_allow: false` semantics elsewhere.

Report outcomes to `POST /v1/feedback` (with the decision id) to feed the
phase 6 learning loop.
