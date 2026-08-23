# ADR-0013: Telemetry schema

## Status

Accepted

## Context

Every decision must be auditable (EU AI Act Article 12 style logging, GDPR
Article 30 records) without ever persisting prompts.

## Decision

- Tracing: `tracing` + `tracing-opentelemetry` + `opentelemetry-otlp`
  (gRPC / tonic) when `--otlp-endpoint` (or `OTEL_EXPORTER_OTLP_ENDPOINT`) is
  set. Every decision emits a `routed.decision` span with attributes for every
  `Decision` field except prompt text: `routed.decision.id`, `.outcome`,
  `.policy`, `.requested_model`, `.selected_tier`, `.gateway_model`,
  `.data_class`, `.task`, `.complexity`, `.risk_score`, `.pii_entities`,
  `.estimated_cost_eur`, `.estimated_savings_eur`, `.snapshot_hash`,
  `.fallback`, `.degraded`, `.dry_run`, `.candidates` (compact JSON),
  `.latency_ms`. Inbound W3C `traceparent` is honoured and propagated upstream.
- Prompts are never logged or attached to spans. With
  `--log-prompt-hashes` a salted SHA-256 of the classified text is recorded as
  `routed.prompt.hash` (salt from `ROUTED_PROMPT_HASH_SALT`).
- Metrics are exposed on `/metrics` in Prometheus text format via
  `prometheus-client`: `routed_decisions_total{outcome,policy,tier,data_class}`,
  `routed_decision_latency_seconds`, `routed_estimated_cost_eur_total{tier}`,
  `routed_estimated_savings_eur_total`, `routed_classifier_latency_seconds{classifier}`,
  `routed_classifier_errors_total{classifier,kind}`, `routed_blocked_total{reason}`,
  `routed_snapshot_age_seconds`, `routed_upstream_requests_total{status}`,
  `routed_hint_ignored_total{kind}`. OTLP metrics export is deferred (the
  Prometheus endpoint is scraped by the OTel collector).
- Logs: JSON via `tracing-subscriber`, one line per decision at `info` with the
  same fields as the span; `RUST_LOG` controls verbosity.

## Consequences

- A collector receiving the spans has a complete, prompt-free record of every
  decision and its alternatives.
- Cardinality: `policy`, `tier` and `data_class` are bounded by configuration;
  no request-derived labels.

## Alternatives considered

- OpenTelemetry metrics API + `opentelemetry-prometheus`: deferred until the
  crates' release trains align; `prometheus-client` is stable today.
