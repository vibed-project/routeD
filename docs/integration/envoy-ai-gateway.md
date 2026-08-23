# Envoy AI Gateway / plain Envoy (ext_proc)

routeD joins the Envoy filter chain as an external processor
(`routed serve --mode extproc`, ADR-0017): the request is mutated in place,
no extra hop, and response streaming never passes through routeD.

## Filter configuration

The contract (ADR-0017) requires this processing mode, mode overrides
enabled, and fail-closed behaviour:

```yaml
http_filters:
  - name: envoy.filters.http.ext_proc
    typed_config:
      "@type": type.googleapis.com/envoy.extensions.filters.http.ext_proc.v3.ExternalProcessor
      grpc_service:
        envoy_grpc:
          cluster_name: routed_extproc
        timeout: 5s
      failure_mode_allow: false      # fail closed when routeD is unreachable
      allow_mode_override: true      # lets routeD skip phases for pass-through traffic
      message_timeout: 2s            # body phase includes classification
      processing_mode:
        request_header_mode: SEND
        request_body_mode: BUFFERED
        response_header_mode: SEND
        response_body_mode: NONE
  - name: envoy.filters.http.router
    typed_config:
      "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
```

The `routed_extproc` cluster must use HTTP/2:

```yaml
clusters:
  - name: routed_extproc
    type: STRICT_DNS
    typed_extension_protocol_options:
      envoy.extensions.upstreams.http.v3.HttpProtocolOptions:
        "@type": type.googleapis.com/envoy.extensions.upstreams.http.v3.HttpProtocolOptions
        explicit_http_config:
          http2_protocol_options: {}
    load_assignment:
      cluster_name: routed_extproc
      endpoints:
        - lb_endpoints:
            - endpoint:
                address:
                  socket_address: { address: routed, port_value: 9002 }
```

With Envoy AI Gateway, attach the same `ExternalProcessor` config through
its filter extension points; the routeD side is identical.

## What routeD does per request

- `POST` to `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`,
  `/v1/messages` or `/v1/responses`: the buffered body is classified and
  decided. `ROUTE` rewrites `model` (and reasoning parameters) in place;
  `BLOCK` answers 403 with the OpenAI error envelope; dry-run answers with
  the decision JSON. The caller sees `X-Routed-*` response headers.
- Everything else: inbound `x-routed-*` headers are stripped (ADR-0007) and
  the remaining phases are skipped via `mode_override`.

The kind e2e (`test/e2e/run.sh test-extproc`) runs exactly this topology
with a real Envoy and asserts the behaviour above.
