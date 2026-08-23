// SPDX-License-Identifier: Apache-2.0
//! `/healthz`, `/readyz` and `/metrics` for the operator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

/// Operator-specific metrics.
pub struct Metrics {
    /// Reconcile cycles completed (success or failure).
    pub reconciles_total: Counter,
    /// Compile errors observed in the most recent reconcile.
    pub compile_errors: Gauge,
    /// `1` while this replica holds the leader lease (always `1` when
    /// `--leader-elect` is off).
    pub is_leader: Gauge,
}

impl Metrics {
    /// Register every metric with `registry`.
    #[must_use]
    pub fn register(registry: &mut Registry) -> Self {
        let reconciles_total = Counter::default();
        registry.register(
            "routed_operator_reconciles",
            "Reconcile cycles completed",
            reconciles_total.clone(),
        );
        let compile_errors = Gauge::default();
        registry.register(
            "routed_operator_compile_errors",
            "Compile errors in the most recent reconcile",
            compile_errors.clone(),
        );
        let is_leader = Gauge::default();
        registry.register(
            "routed_operator_is_leader",
            "1 if this replica currently holds the leader lease",
            is_leader.clone(),
        );
        Self {
            reconciles_total,
            compile_errors,
            is_leader,
        }
    }
}

/// Shared state for the health/metrics HTTP servers.
#[derive(Clone)]
pub struct AppState {
    /// Set once the first snapshot compiles successfully.
    pub ready: Arc<AtomicBool>,
    /// Metrics registry, encoded on `/metrics`.
    pub registry: Arc<Registry>,
}

/// `/healthz` and `/readyz`.
pub fn health_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// `/metrics`.
pub fn metrics_router(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let mut buf = String::new();
    if encode(&mut buf, &state.registry).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
    }
    (StatusCode::OK, buf)
}
