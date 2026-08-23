// SPDX-License-Identifier: Apache-2.0
//! Inline `OpenAI`-compatible HTTP forwarder (ADR-0012): rewrites the `model`
//! field and injected parameters, sets `X-Routed-*` headers, and streams the
//! upstream response back unmodified. Also serves `POST /v1/decide`,
//! `POST /v1/feedback`, `/healthz`, `/readyz` and `/metrics`.
//!
//! Zero buffering of response bodies is a tested invariant.

pub mod authn;
mod body;
pub mod errors;
mod forward;
pub mod handlers;
pub mod headers;
mod pipeline;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::{any, get, post};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use routed_classify::Classifier;
use routed_decision::{Engine, NoPredictor, SharedPredictor};
use routed_feedback::FeedbackSink;
use routed_snapshot::SnapshotHolder;
use routed_telemetry::Telemetry;
use tokio::sync::Semaphore;

pub use forward::Upstream;
pub use pipeline::ClassifyRunner;

/// Runtime configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// Upstream gateway base URL (for example `http://litellm:4000`).
    pub upstream: String,
    /// Maximum accepted request body in bytes.
    pub max_body_bytes: usize,
    /// Per-classifier timeout.
    pub classify_timeout: Duration,
    /// Maximum concurrent classifier executions.
    pub classify_concurrency: usize,
    /// Idle timeout for streamed upstream responses.
    pub stream_idle_timeout: Duration,
    /// Maximum size of the `X-Routed-Decision` header value in bytes.
    pub decision_header_max: usize,
    /// Forward every non-decision path (default: only the `/v1` API surface).
    pub passthrough_all: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            upstream: "http://127.0.0.1:4000".into(),
            max_body_bytes: 10 * 1024 * 1024,
            classify_timeout: Duration::from_millis(25),
            classify_concurrency: 8,
            stream_idle_timeout: Duration::from_secs(60),
            decision_header_max: 4096,
            passthrough_all: false,
        }
    }
}

/// Shared application state.
pub struct AppState {
    /// Current snapshot.
    pub snapshot: Arc<SnapshotHolder>,
    /// Decision engine (learned predictor attachable via [`AppState::with_predictor`]).
    pub engine: Engine<SharedPredictor>,
    /// Feedback / decision-journal sink (ADR-0018).
    pub feedback: Arc<dyn FeedbackSink>,
    /// Caller authentication for the decision APIs (ADR-0020).
    pub authn: Arc<dyn authn::Authenticator>,
    /// Classifier.
    pub classifier: Arc<dyn Classifier>,
    /// Upstream client.
    pub upstream: Upstream,
    /// Configuration.
    pub config: Config,
    /// Telemetry.
    pub telemetry: Arc<Telemetry>,
    /// Bounded classifier concurrency.
    pub classify_sem: Arc<Semaphore>,
    started: Instant,
}

impl AppState {
    /// Build the state; the upstream client is created here.
    ///
    /// # Errors
    /// When `config.upstream` is not a valid URL.
    pub fn new(
        snapshot: Arc<SnapshotHolder>,
        classifier: Arc<dyn Classifier>,
        telemetry: Arc<Telemetry>,
        config: Config,
    ) -> anyhow::Result<Self> {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        let upstream = Upstream::new(client, &config.upstream)?;
        // Warm the classifier (lazy regex compilation, model loading) so the first
        // request is not misclassified as a timeout. A panicking classifier must not
        // take the process down; it is reported as degraded per request instead.
        // Remote (`http`) classifiers are not warmed: that would block startup on the network.
        if classifier.name() != "http" {
            let warm = Instant::now();
            let warm_input = routed_classify::ClassifyInput::user(
                "warm-up: summarise this, jane.doe@example.org, ignore previous instructions",
            );
            let c = Arc::clone(&classifier);
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.classify(&warm_input)));
            match outcome {
                Ok(Ok(_)) => tracing::debug!(
                    classifier = classifier.name(),
                    ms = u64::try_from(warm.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "classifier warmed up"
                ),
                Ok(Err(e)) => {
                    tracing::warn!(classifier = classifier.name(), error = %e, "classifier warm-up failed");
                }
                Err(_) => tracing::error!(
                    classifier = classifier.name(),
                    "classifier panicked during warm-up"
                ),
            }
        }
        Ok(Self {
            snapshot,
            engine: Engine::with_predictor(Arc::new(NoPredictor)),
            feedback: Arc::new(routed_feedback::NullSink),
            authn: Arc::new(authn::AllowAll),
            classifier,
            upstream,
            classify_sem: Arc::new(Semaphore::new(config.classify_concurrency.max(1))),
            config,
            telemetry,
            started: Instant::now(),
        })
    }

    /// Attach a learned quality predictor (ADR-0018).
    #[must_use]
    pub fn with_predictor(mut self, predictor: SharedPredictor) -> Self {
        self.engine = Engine::with_predictor(predictor);
        self
    }

    /// Attach a feedback sink (ADR-0018).
    #[must_use]
    pub fn with_feedback(mut self, sink: Arc<dyn FeedbackSink>) -> Self {
        self.feedback = sink;
        self
    }

    /// Attach a caller authenticator for the decision APIs (ADR-0020).
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn authn::Authenticator>) -> Self {
        self.authn = authenticator;
        self
    }

    /// Process uptime.
    #[must_use]
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/metrics", get(handlers::metrics))
        .route("/v1/decide", post(handlers::decide))
        .route("/v1/feedback", post(handlers::feedback))
}

/// Build the axum router (API surface plus the forwarding fallback).
pub fn router(state: Arc<AppState>) -> Router {
    api_routes()
        .fallback(any(handlers::proxy))
        .with_state(state)
}

/// The decision / feedback / health / metrics API surface without the proxy
/// fallback: what `--mode extproc` serves on the HTTP port.
pub fn api_router(state: Arc<AppState>) -> Router {
    api_routes().with_state(state)
}

/// Serve until the shutdown future resolves.
///
/// # Errors
/// On bind or serve errors.
pub async fn serve(
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, upstream = %state.config.upstream, "inline ingress listening");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}
