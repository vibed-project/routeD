# Observability

- `grafana-dashboard.json`: import into Grafana (Prometheus data source) for decisions, outcomes, tiers, latency, cost and savings.
- routeD exposes Prometheus metrics on `/metrics` and exports `routed.decision` spans over OTLP when `OTEL_EXPORTER_OTLP_ENDPOINT` is set (see `docs/adr/0013-telemetry-schema.md`).
