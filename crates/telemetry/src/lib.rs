// SPDX-License-Identifier: Apache-2.0
//! `OpenTelemetry` setup, Prometheus metrics, decision span / log helpers, and
//! structured JSON logging (ADR-0013).
//!
//! Prompts are never logged; only a salted hash when explicitly enabled.

use std::sync::Mutex;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use prometheus_client::encoding::{EncodeLabelSet, text::encode};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use routed_decision::{Decision, Outcome};
use sha2::{Digest, Sha256};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Truncate client-controlled strings before they reach logs / spans.
#[must_use]
pub fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Telemetry configuration.
#[derive(Clone, Debug, Default)]
pub struct TelemetryConfig {
    /// OTLP gRPC endpoint (for example `http://otel-collector:4317`). `None` disables tracing export.
    pub otlp_endpoint: Option<String>,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Record a salted hash of the classified text on each decision.
    pub prompt_hashes: bool,
    /// Salt for prompt hashes.
    pub prompt_hash_salt: String,
}

/// Initialise JSON structured logging with `RUST_LOG`-style filtering (default `info`).
///
/// Safe to call once per process; later calls are ignored. Use [`init`] for
/// the full stack (logging + tracing export + metrics).
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_target(true))
        .try_init();
}

/// Decision metric labels (bounded cardinality: configuration-derived only).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DecisionLabels {
    /// Outcome.
    pub outcome: String,
    /// Policy key.
    pub policy: String,
    /// Selected tier.
    pub tier: String,
    /// Data class.
    pub data_class: String,
    /// `inline` (forwarded), `decide` (decision API) or `dry-run`.
    pub mode: String,
}

/// Single string label.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct NameLabel {
    /// Label value.
    pub name: String,
}

/// Classifier error labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ClassifierErrorLabels {
    /// Classifier name.
    pub classifier: String,
    /// Error kind.
    pub kind: String,
}

/// Upstream status labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StatusLabels {
    /// HTTP status class (`2xx`, `4xx`, ...).
    pub status: String,
}

/// All routeD metrics.
pub struct Metrics {
    /// `routed_decisions_total`.
    pub decisions_total: Family<DecisionLabels, Counter>,
    /// `routed_decision_latency_seconds`.
    pub decision_latency: Histogram,
    /// `routed_estimated_cost_eur_total{tier}`.
    pub cost_eur_total: Family<NameLabel, Counter<f64>>,
    /// `routed_estimated_savings_eur_total`.
    pub savings_eur_total: Counter<f64>,
    /// `routed_classifier_latency_seconds{classifier}`.
    pub classifier_latency: Family<NameLabel, Histogram>,
    /// `routed_classifier_errors_total{classifier,kind}`.
    pub classifier_errors_total: Family<ClassifierErrorLabels, Counter>,
    /// `routed_blocked_total{reason}`.
    pub blocked_total: Family<NameLabel, Counter>,
    /// `routed_snapshot_age_seconds`.
    pub snapshot_age_seconds: Gauge,
    /// `routed_upstream_requests_total{status}`.
    pub upstream_requests_total: Family<StatusLabels, Counter>,
    /// `routed_hint_ignored_total{kind}`.
    pub hint_ignored_total: Family<NameLabel, Counter>,
    /// `routed_requests_rejected_total{reason}` (oversize, malformed, not ready).
    pub requests_rejected_total: Family<NameLabel, Counter>,
}

impl Metrics {
    fn register(registry: &mut Registry) -> Self {
        let decisions_total = Family::<DecisionLabels, Counter>::default();
        registry.register(
            "routed_decisions",
            "Decisions by outcome, policy, tier and data class",
            decisions_total.clone(),
        );
        let decision_latency = Histogram::new(exponential_buckets(0.0001, 2.0, 16));
        registry.register(
            "routed_decision_latency_seconds",
            "Decision latency including classification",
            decision_latency.clone(),
        );
        let cost_eur_total = Family::<NameLabel, Counter<f64>>::default();
        registry.register(
            "routed_estimated_cost_eur",
            "Estimated cost of routed requests in EUR",
            cost_eur_total.clone(),
        );
        let savings_eur_total = Counter::<f64>::default();
        registry.register(
            "routed_estimated_savings_eur",
            "Estimated savings versus the most expensive surviving candidate in EUR",
            savings_eur_total.clone(),
        );
        let classifier_latency = Family::<NameLabel, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.0005, 2.0, 14))
        });
        registry.register(
            "routed_classifier_latency_seconds",
            "Classifier latency",
            classifier_latency.clone(),
        );
        let classifier_errors_total = Family::<ClassifierErrorLabels, Counter>::default();
        registry.register(
            "routed_classifier_errors",
            "Classifier errors and timeouts",
            classifier_errors_total.clone(),
        );
        let blocked_total = Family::<NameLabel, Counter>::default();
        registry.register(
            "routed_blocked",
            "Blocked requests by reason",
            blocked_total.clone(),
        );
        let snapshot_age_seconds = Gauge::default();
        registry.register(
            "routed_snapshot_age_seconds",
            "Seconds since the current snapshot was loaded",
            snapshot_age_seconds.clone(),
        );
        let upstream_requests_total = Family::<StatusLabels, Counter>::default();
        registry.register(
            "routed_upstream_requests",
            "Upstream responses by status class",
            upstream_requests_total.clone(),
        );
        let hint_ignored_total = Family::<NameLabel, Counter>::default();
        registry.register(
            "routed_hint_ignored",
            "Ignored request hints by kind",
            hint_ignored_total.clone(),
        );
        let requests_rejected_total = Family::<NameLabel, Counter>::default();
        registry.register(
            "routed_requests_rejected",
            "Requests rejected before a decision",
            requests_rejected_total.clone(),
        );
        Self {
            decisions_total,
            decision_latency,
            cost_eur_total,
            savings_eur_total,
            classifier_latency,
            classifier_errors_total,
            blocked_total,
            snapshot_age_seconds,
            upstream_requests_total,
            hint_ignored_total,
            requests_rejected_total,
        }
    }
}

/// Initialised telemetry stack.
pub struct Telemetry {
    /// Metrics.
    pub metrics: Metrics,
    registry: Registry,
    provider: Mutex<Option<SdkTracerProvider>>,
    config: TelemetryConfig,
}

impl Telemetry {
    /// Initialise logging, optional OTLP tracing export and the metrics registry.
    ///
    /// # Errors
    /// When the OTLP exporter cannot be built.
    pub fn init(config: TelemetryConfig) -> anyhow::Result<Self> {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let fmt_layer = fmt::layer().json().with_target(true);
        let mut provider = None;
        global::set_text_map_propagator(TraceContextPropagator::new());
        if let Some(endpoint) = &config.otlp_endpoint {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .build()?;
            let resource = opentelemetry_sdk::Resource::builder()
                .with_service_name(config.service_name.clone())
                .build();
            let p = SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource)
                .build();
            let tracer = p.tracer("routed");
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .try_init();
            provider = Some(p);
        } else {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .try_init();
        }
        let mut registry = Registry::default();
        let metrics = Metrics::register(&mut registry);
        Ok(Self {
            metrics,
            registry,
            provider: Mutex::new(provider),
            config,
        })
    }

    /// Metrics-only instance for tests (no logging / export setup).
    #[must_use]
    pub fn for_tests() -> Self {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let mut registry = Registry::default();
        let metrics = Metrics::register(&mut registry);
        Self {
            metrics,
            registry,
            provider: Mutex::new(None),
            config: TelemetryConfig::default(),
        }
    }

    /// Prometheus text exposition.
    ///
    /// # Errors
    /// Never in practice.
    pub fn encode_metrics(&self) -> anyhow::Result<String> {
        let mut out = String::new();
        encode(&mut out, &self.registry)?;
        Ok(out)
    }

    /// Flush and shut down the trace exporter.
    pub fn shutdown(&self) {
        if let Some(p) = self.provider.lock().ok().and_then(|mut g| g.take()) {
            let _ = p.shutdown();
        }
    }

    /// Salted prompt hash, if enabled.
    #[must_use]
    pub fn prompt_hash(&self, text: &str) -> Option<String> {
        if !self.config.prompt_hashes {
            return None;
        }
        let mut h = Sha256::new();
        h.update(self.config.prompt_hash_salt.as_bytes());
        h.update(text.as_bytes());
        Some(hex::encode(h.finalize()))
    }

    /// Record a decision: metrics, span attributes on the current span, and one log line.
    /// `mode` is `inline` for forwarded requests or `decide` for the decision API; dry
    /// runs are recorded as `dry-run`. Cost and savings counters only count forwarded requests.
    pub fn record_decision(
        &self,
        d: &Decision,
        latency: Duration,
        prompt_hash: Option<&str>,
        mode: &str,
    ) {
        let m = &self.metrics;
        let mode = if d.dry_run { "dry-run" } else { mode };
        let requested_model = cap(&d.requested_model, 128);
        m.decisions_total
            .get_or_create(&DecisionLabels {
                outcome: d.outcome.to_string(),
                policy: d.policy.clone().unwrap_or_default(),
                tier: d.selected_tier.clone().unwrap_or_default(),
                data_class: d.data_class.clone().unwrap_or_default(),
                mode: mode.to_owned(),
            })
            .inc();
        m.decision_latency.observe(latency.as_secs_f64());
        if mode == "inline" {
            if let Some(c) = d.estimated_cost_eur {
                m.cost_eur_total
                    .get_or_create(&NameLabel {
                        name: d.selected_tier.clone().unwrap_or_default(),
                    })
                    .inc_by(c);
            }
            if let Some(s) = d.estimated_savings_eur {
                m.savings_eur_total.inc_by(s);
            }
        }
        if d.outcome == Outcome::Block {
            let reason = d
                .candidates
                .iter()
                .find_map(|c| c.eliminated_by)
                .map_or_else(|| "no-candidate".to_string(), |r| r.to_string());
            m.blocked_total
                .get_or_create(&NameLabel { name: reason })
                .inc();
        }
        for n in &d.notes {
            let kind = if n.contains("Data-Class") {
                "data-class"
            } else if n.contains("Policy") {
                "policy"
            } else {
                "other"
            };
            m.hint_ignored_total
                .get_or_create(&NameLabel { name: kind.into() })
                .inc();
        }

        let span = tracing::Span::current();
        span.set_attribute("routed.decision.id", d.id.clone());
        span.set_attribute("routed.decision.outcome", d.outcome.to_string());
        span.set_attribute(
            "routed.decision.policy",
            d.policy.clone().unwrap_or_default(),
        );
        span.set_attribute("routed.decision.requested_model", requested_model.clone());
        span.set_attribute(
            "routed.decision.selected_tier",
            d.selected_tier.clone().unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.gateway_model",
            d.gateway_model.clone().unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.data_class",
            d.data_class.clone().unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.task",
            d.classification.task.clone().unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.complexity",
            d.classification
                .complexity
                .map(|c| format!("{c:?}").to_lowercase())
                .unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.risk_score",
            d.classification.risk_score.unwrap_or(-1.0),
        );
        span.set_attribute(
            "routed.decision.pii_entities",
            serde_json::to_string(&d.classification.pii_entities).unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.estimated_cost_eur",
            d.estimated_cost_eur.unwrap_or(0.0),
        );
        span.set_attribute(
            "routed.decision.estimated_savings_eur",
            d.estimated_savings_eur.unwrap_or(0.0),
        );
        span.set_attribute("routed.decision.snapshot_hash", d.snapshot_hash.clone());
        span.set_attribute("routed.decision.fallback", d.fallback);
        span.set_attribute("routed.decision.dry_run", d.dry_run);
        span.set_attribute("routed.decision.degraded", d.degraded.join(","));
        span.set_attribute(
            "routed.decision.candidates",
            serde_json::to_string(&d.candidates).unwrap_or_default(),
        );
        span.set_attribute(
            "routed.decision.latency_ms",
            i64::try_from(latency.as_millis()).unwrap_or(i64::MAX),
        );
        if let Some(h) = prompt_hash {
            span.set_attribute("routed.prompt.hash", h.to_owned());
        }
        tracing::info!(
            target: "routed.decision",
            id = %d.id,
            outcome = %d.outcome,
            policy = d.policy.as_deref().unwrap_or(""),
            requested_model = %requested_model,
            selected_tier = d.selected_tier.as_deref().unwrap_or(""),
            gateway_model = d.gateway_model.as_deref().unwrap_or(""),
            data_class = d.data_class.as_deref().unwrap_or(""),
            risk_score = d.classification.risk_score.unwrap_or(-1.0),
            estimated_cost_eur = d.estimated_cost_eur.unwrap_or(0.0),
            estimated_savings_eur = d.estimated_savings_eur.unwrap_or(0.0),
            snapshot_hash = %d.snapshot_hash,
            fallback = d.fallback,
            latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
            "decision"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init_tracing();
        init_tracing();
    }

    #[test]
    fn metrics_encode() {
        let t = Telemetry::for_tests();
        let decision = Decision {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            outcome: Outcome::Route,
            policy: None,
            requested_model: "auto".to_owned(),
            selected_tier: None,
            gateway_model: None,
            parameters: routed_decision::Parameters::default(),
            data_class: None,
            classification: routed_decision::Classification::default(),
            candidates: Vec::new(),
            estimated_cost_eur: None,
            estimated_savings_eur: None,
            latency_ms: 0,
            snapshot_hash: "test".to_owned(),
            reason: None,
            fallback: false,
            degraded: Vec::new(),
            notes: Vec::new(),
            dry_run: false,
        };
        t.record_decision(&decision, Duration::from_millis(1), None, "decide");
        let text = t.encode_metrics().unwrap_or_default();
        assert!(text.contains("routed_decisions_total"));
        assert!(t.prompt_hash("x").is_none());
    }
}
