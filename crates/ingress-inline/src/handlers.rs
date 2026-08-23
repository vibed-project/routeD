// SPDX-License-Identifier: Apache-2.0
//! HTTP handlers.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use routed_decision::{DecisionContext, Outcome, is_routed};
use routed_router::{ParsedRequest, parse_request};
use routed_security::extract_headers;
use routed_telemetry::NameLabel;
use serde_json::value::RawValue;

use crate::errors::{bad_gateway, bad_request, error_response, not_ready, too_large};
use crate::pipeline::ClassifyRunner;
use crate::{AppState, headers};

/// Paths routeD makes decisions for.
/// Normalise a request path for matching: collapse repeated slashes, drop a
/// trailing slash, and reject dot segments or percent-encoded slashes / dots
/// (never legitimate API paths; they would otherwise dodge the decision while an
/// upstream normalises them back).
#[must_use]
pub fn normalize_path(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%2e") || lower.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    for seg in raw.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." {
            return None;
        }
        out.push('/');
        out.push_str(seg);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// Whether a non-decision path may be forwarded (the `OpenAI` API surface only by default).
#[must_use]
pub fn passthrough_allowed(path: &str, allow_all: bool) -> bool {
    allow_all || path == "/v1" || path.starts_with("/v1/")
}

/// Decision paths.
pub const ROUTED_PATHS: [&str; 5] = [
    "/v1/chat/completions",
    "/v1/completions",
    "/v1/embeddings",
    "/v1/messages",
    "/v1/responses",
];

/// Liveness.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Readiness: a snapshot must be loaded.
pub async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    if state.snapshot.is_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "no snapshot loaded").into_response()
    }
}

/// Prometheus metrics.
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let age = state.snapshot.age().map_or(0, |d| d.as_secs());
    state
        .telemetry
        .metrics
        .snapshot_age_seconds
        .set(i64::try_from(age).unwrap_or(i64::MAX));
    match state.telemetry.encode_metrics() {
        Ok(text) => (
            [(
                header::CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `POST /v1/feedback` (ingestion lands fully in phase 6; events are logged now).
pub async fn feedback(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    if body.len() > 64 * 1024 {
        return too_large(64 * 1024);
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return bad_request("feedback must be JSON");
    };
    let Some(id) = v
        .get("decisionId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| id.len() == 26 && id.chars().all(|c| c.is_ascii_alphanumeric()))
    else {
        return bad_request("feedback requires a ULID decisionId");
    };
    let source = v
        .get("source")
        .and_then(serde_json::Value::as_str)
        .filter(|s| matches!(*s, "user" | "agent" | "gateway"))
        .unwrap_or("unknown");
    let outcome = v
        .get("outcome")
        .filter(|o| o.is_object())
        .cloned()
        .unwrap_or_default();
    tracing::info!(target: "routed.feedback", decision_id = id, source, outcome = %routed_telemetry::cap(&outcome.to_string(), 512), "feedback");
    state
        .feedback
        .record_feedback(routed_feedback::FeedbackRecord {
            decision_id: id.to_owned(),
            ts: routed_feedback::now_rfc3339(),
            source: source.to_owned(),
            outcome,
        });
    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"accepted"}"#,
    )
        .into_response()
}

/// A made decision plus what the pipeline learned on the way (shared with the
/// `ext_proc` ingress, which applies it through Envoy mutations instead of a
/// forwarded request).
pub struct Decided {
    /// The decision.
    pub decision: routed_decision::Decision,
    /// Parsed request facts.
    pub parsed: ParsedRequest,
    /// Whether the matched policy wants the explanation header.
    pub explain: bool,
}

async fn read_body(
    req: Request,
    limit: usize,
) -> Result<(axum::http::request::Parts, axum::body::Bytes), Response> {
    let (parts, body) = req.into_parts();
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        axum::body::to_bytes(body, limit),
    )
    .await
    {
        Ok(Ok(b)) => Ok((parts, b)),
        Ok(Err(e)) => {
            if e.into_inner()
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                Err(too_large(limit))
            } else {
                Err(bad_request("request body could not be read"))
            }
        }
        Err(_) => Err(error_response(
            StatusCode::REQUEST_TIMEOUT,
            "invalid_request_error",
            "routed_body_timeout",
            "request body was not received within 30s",
            None,
        )),
    }
}

fn header_pairs(h: &HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect()
}

/// Run the full decision pipeline over raw bytes: not-ready / parse guards,
/// classification (skipped for pass-through), engine, telemetry. Errors are
/// ready-to-send OpenAI-shaped responses.
///
/// # Errors
/// A complete error [`Response`] (not ready, malformed, too large).
pub async fn decide_bytes(
    state: &AppState,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
    mode: &str,
) -> Result<Decided, Response> {
    let Some(snapshot) = state.snapshot.load() else {
        state
            .telemetry
            .metrics
            .requests_rejected_total
            .get_or_create(&NameLabel {
                name: "not-ready".into(),
            })
            .inc();
        return Err(not_ready());
    };
    let started = Instant::now();
    let req_headers = extract_headers(header_pairs(headers));
    let parsed = match parse_request(path, body) {
        Ok(p) => p,
        Err(e) => {
            state
                .telemetry
                .metrics
                .requests_rejected_total
                .get_or_create(&NameLabel {
                    name: "malformed".into(),
                })
                .inc();
            return Err(bad_request(&e.to_string()));
        }
    };
    let default_output = snapshot
        .core
        .profiles
        .get("default")
        .or_else(|| snapshot.core.profiles.values().next())
        .map_or(256, |p| p.default_output_tokens);
    let input = parsed.to_input(path, &req_headers, default_output);
    let ctx = DecisionContext {
        id: new_decision_id(),
    };
    // Skip classification entirely when the request will pass through anyway.
    let findings = if is_routed(&snapshot, &input) {
        let runner = ClassifyRunner {
            classifier: Arc::clone(&state.classifier),
            semaphore: Arc::clone(&state.classify_sem),
            timeout: state.config.classify_timeout,
            telemetry: Arc::clone(&state.telemetry),
        };
        runner.run(parsed.classify_input.clone()).await
    } else {
        routed_decision::Findings {
            risk_score: Some(0.0),
            ..Default::default()
        }
    };
    let mut decision = state.engine.decide(&snapshot, &input, &findings, &ctx);
    for ignored in &req_headers.ignored {
        decision.notes.push(format!("ignored header {ignored}"));
    }
    let latency = started.elapsed();
    decision.latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
    let hash = state
        .telemetry
        .prompt_hash(&parsed.classify_input.user_text);
    state
        .telemetry
        .record_decision(&decision, latency, hash.as_deref(), mode);
    state
        .feedback
        .record_decision(routed_feedback::DecisionRecord::from_decision(
            &decision,
            mode,
            routed_feedback::now_rfc3339(),
        ));
    let explain = decision
        .policy
        .as_deref()
        .and_then(|k| snapshot.policy(k))
        .is_none_or(|p| p.explain);
    Ok(Decided {
        decision,
        parsed,
        explain,
    })
}

/// `POST /v1/decide`: returns the decision JSON, never forwards.
#[tracing::instrument(name = "routed.decision", skip_all, fields(routed.mode = "decide"))]
pub async fn decide(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let limit = state.config.max_body_bytes;
    let path = match req
        .headers()
        .get("x-routed-path")
        .and_then(|v| v.to_str().ok())
    {
        None => "/v1/chat/completions".to_owned(),
        Some(p) => match normalize_path(p) {
            Some(n) if ROUTED_PATHS.contains(&n.as_str()) => n,
            _ => return bad_request("X-Routed-Path must be one of the routed OpenAI paths"),
        },
    };
    let (parts, body) = match read_body(req, limit).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    match decide_bytes(&state, &path, &parts.headers, &body, "decide").await {
        Ok(d) => {
            let mut resp = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                d.decision.to_json(),
            )
                .into_response();
            headers::apply(
                resp.headers_mut(),
                &d.decision,
                d.explain,
                state.config.decision_header_max,
            );
            resp
        }
        Err(r) => r,
    }
}

/// Time-ordered unique decision id (ULID: 48-bit ms timestamp + 80 random bits).
fn new_decision_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let random: u128 = rand::random::<u128>() & ((1u128 << 80) - 1);
    ulid::Ulid::from_parts(ms, random).to_string()
}

/// The BLOCK response: an OpenAI-shaped 403 carrying the decision id, policy,
/// elimination reasons and snapshot hash, plus the `X-Routed-*` headers. One
/// envelope for both ingress modes.
pub fn block_response(d: &routed_decision::Decision, explain: bool, hmax: usize) -> Response {
    let reasons: Vec<String> = d
        .candidates
        .iter()
        .filter_map(|c| c.eliminated_by.map(|r| r.to_string()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut resp = error_response(
        StatusCode::FORBIDDEN,
        "invalid_request_error",
        "routed_policy_blocked",
        d.reason.as_deref().unwrap_or("blocked by routing policy"),
        Some(
            serde_json::json!({ "decisionId": d.id, "policy": d.policy, "reasons": reasons, "snapshotHash": d.snapshot_hash }),
        ),
    );
    headers::apply(resp.headers_mut(), d, explain, hmax);
    resp
}

/// Rewrite `model` and injected parameters, preserving every other field byte for byte.
///
/// # Errors
/// A human-readable reason when the body is not a JSON object.
pub fn rewrite_body(
    path: &str,
    body: &[u8],
    d: &routed_decision::Decision,
) -> Result<Vec<u8>, String> {
    let mut map: indexmap::IndexMap<String, serde_json::Value> = serde_json::from_slice::<
        indexmap::IndexMap<String, Box<RawValue>>,
    >(body)
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(k, v): (String, Box<RawValue>)| (k, serde_json::Value::String(v.get().to_owned())))
    .collect();
    // Values are kept as raw JSON text (wrapped in a String only for the map); re-emit manually.
    if let Some(model) = &d.gateway_model {
        map.insert(
            "model".into(),
            serde_json::Value::String(serde_json::to_string(model).map_err(|e| e.to_string())?),
        );
    }
    if let Some(level) = d.parameters.reasoning {
        let effort = match level {
            routed_decision::ReasoningLevel::None => None,
            routed_decision::ReasoningLevel::Low => Some("low"),
            routed_decision::ReasoningLevel::Medium => Some("medium"),
            routed_decision::ReasoningLevel::High => Some("high"),
        };
        match (path, effort) {
            ("/v1/chat/completions", Some(e)) => {
                map.insert(
                    "reasoning_effort".into(),
                    serde_json::Value::String(format!("\"{e}\"")),
                );
            }
            ("/v1/chat/completions", None) => {
                map.shift_remove("reasoning_effort");
            }
            ("/v1/responses", Some(e)) => {
                // Merge into an existing reasoning object (keeps e.g. `summary`).
                let mut obj: serde_json::Map<String, serde_json::Value> = map
                    .get("reasoning")
                    .and_then(|v| {
                        if let serde_json::Value::String(raw) = v {
                            serde_json::from_str(raw).ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                obj.insert("effort".into(), serde_json::Value::String(e.into()));
                map.insert(
                    "reasoning".into(),
                    serde_json::Value::String(serde_json::Value::Object(obj).to_string()),
                );
            }
            ("/v1/responses", None) => {
                map.shift_remove("reasoning");
            }
            _ => {}
        }
    }
    if let Some(max) = d.parameters.max_tokens {
        let key = match path {
            "/v1/responses" => "max_output_tokens",
            "/v1/chat/completions" if map.contains_key("max_completion_tokens") => {
                "max_completion_tokens"
            }
            "/v1/embeddings" => "",
            _ => "max_tokens",
        };
        if !key.is_empty() {
            map.insert(key.into(), serde_json::Value::String(max.to_string()));
        }
    }
    // `shift_remove` keeps the original order of the remaining fields.
    let _ = &mut map;
    let mut out = Vec::with_capacity(body.len() + 64);
    out.push(b'{');
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(
            serde_json::to_string(k)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        );
        out.push(b':');
        if let serde_json::Value::String(raw) = v {
            out.extend_from_slice(raw.as_bytes());
        }
    }
    out.push(b'}');
    Ok(out)
}

/// Every other request: decide + forward for routed paths, verbatim proxy otherwise.
#[tracing::instrument(name = "routed.decision", skip_all, fields(routed.mode = "inline", http.path = %req.uri().path()))]
pub async fn proxy(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let Some(path) = normalize_path(req.uri().path()) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "routed_invalid_path",
            "path contains dot segments or encoded separators",
            None,
        );
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let path_and_query = format!("{path}{query}");
    let idle = state.config.stream_idle_timeout;
    if req.method() != Method::POST || !ROUTED_PATHS.contains(&path.as_str()) {
        if !passthrough_allowed(&path, state.config.passthrough_all) {
            return error_response(
                StatusCode::NOT_FOUND,
                "invalid_request_error",
                "routed_path_not_allowed",
                "only the /v1 API surface is forwarded (set --passthrough-all to forward everything)",
                None,
            );
        }
        // Verbatim pass-through, streamed both ways.
        let (parts, body) = req.into_parts();
        let len = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        return match state
            .upstream
            .forward(
                parts.method,
                &path_and_query,
                &parts.headers,
                body,
                len,
                idle,
            )
            .await
        {
            Ok(resp) => observe(&state, resp),
            Err(e) => bad_gateway(&e.to_string()),
        };
    }
    let limit = state.config.max_body_bytes;
    let (parts, body) = match read_body(req, limit).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let decided = match decide_bytes(&state, &path, &parts.headers, &body, "inline").await {
        Ok(d) => d,
        Err(r) => return r,
    };
    let d = &decided.decision;
    let hmax = state.config.decision_header_max;
    if d.dry_run {
        let mut resp = (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            d.to_json(),
        )
            .into_response();
        headers::apply(resp.headers_mut(), d, decided.explain, hmax);
        return resp;
    }
    match d.outcome {
        Outcome::Block => block_response(d, decided.explain, hmax),
        Outcome::PassThrough | Outcome::Route => {
            let out_body: Vec<u8> = if d.outcome == Outcome::Route {
                match rewrite_body(&path, &body, d) {
                    Ok(b) => b,
                    Err(e) => return bad_request(&format!("cannot rewrite request: {e}")),
                }
            } else {
                body.to_vec()
            };
            let len = out_body.len() as u64;
            let _ = &decided.parsed;
            match state
                .upstream
                .forward(
                    parts.method,
                    &path_and_query,
                    &parts.headers,
                    Body::from(out_body),
                    Some(len),
                    idle,
                )
                .await
            {
                Ok(mut resp) => {
                    headers::apply(resp.headers_mut(), d, decided.explain, hmax);
                    observe(&state, resp)
                }
                Err(e) => {
                    let mut resp = bad_gateway(&e.to_string());
                    headers::apply(resp.headers_mut(), d, decided.explain, hmax);
                    resp
                }
            }
        }
    }
}

fn observe(state: &AppState, mut resp: Response) -> Response {
    let class = format!("{}xx", resp.status().as_u16() / 100);
    state
        .telemetry
        .metrics
        .upstream_requests_total
        .get_or_create(&routed_telemetry::StatusLabels { status: class })
        .inc();
    // SSE / streaming hygiene: never let intermediaries buffer.
    if resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"))
    {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        resp.headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    resp
}
