// SPDX-License-Identifier: Apache-2.0
//! Mock `OpenAI`-compatible gateway: records every request it receives (model,
//! headers, body) and replays scripted responses, including SSE streams with
//! a gate that holds the stream until released (used to prove routeD never
//! buffers responses).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

/// A recorded request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recorded {
    /// Request path.
    pub path: String,
    /// `model` field if the body was JSON with one.
    pub model: Option<String>,
    /// Headers (lowercase names).
    pub headers: Vec<(String, String)>,
    /// Raw body.
    pub body: String,
    /// `stream` flag if present.
    pub stream: bool,
}

/// Scripted response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Script {
    /// Chunks to send in order (base64 when `base64: true`).
    pub chunks: Vec<String>,
    /// Chunks are base64 encoded.
    #[serde(default)]
    pub base64: bool,
    /// Hold the stream after this many chunks until `/_control/release` is called.
    #[serde(default)]
    pub gate_after: Option<usize>,
    /// Content type (default `text/event-stream`).
    #[serde(default)]
    pub content_type: Option<String>,
    /// HTTP status (default 200).
    #[serde(default)]
    pub status: Option<u16>,
}

/// Shared mock state.
#[derive(Default)]
pub struct MockState {
    /// Recorded requests.
    pub requests: Mutex<Vec<Recorded>>,
    /// Scripted response for streaming requests (or all requests when `force` is set).
    pub script: Mutex<Option<Script>>,
    /// Gate release signal.
    pub release: Notify,
    /// Released flag (so a release before the gate is reached is not lost).
    pub released: std::sync::atomic::AtomicBool,
    /// Number of client disconnects observed mid-stream.
    pub disconnects: AtomicUsize,
    /// Requests seen.
    pub count: AtomicUsize,
}

impl MockState {
    /// Snapshot of recorded requests.
    pub async fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().await.clone()
    }

    /// Install a script.
    pub async fn set_script(&self, script: Script) {
        self.released.store(false, Ordering::SeqCst);
        *self.script.lock().await = Some(script);
    }

    /// Release the gate.
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release.notify_waiters();
    }

    /// Clear recorded requests and scripts.
    pub async fn reset(&self) {
        self.requests.lock().await.clear();
        *self.script.lock().await = None;
        self.released.store(false, Ordering::SeqCst);
        self.disconnects.store(0, Ordering::SeqCst);
    }
}

struct DisconnectGuard {
    state: Arc<MockState>,
    finished: bool,
}

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.state.disconnects.fetch_add(1, Ordering::SeqCst);
        }
    }
}

async fn handle(State(state): State<Arc<MockState>>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let json: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
    let model = json
        .as_ref()
        .and_then(|j| j.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let stream = json
        .as_ref()
        .and_then(|j| j.get("stream"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    state.requests.lock().await.push(Recorded {
        path: path.clone(),
        model: model.clone(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
        stream,
    });
    state.count.fetch_add(1, Ordering::SeqCst);

    let script = state.script.lock().await.clone();
    if let Some(script) = script.filter(|_| stream || path.starts_with("/_script")) {
        return scripted(&state, &script);
    }
    if stream {
        // Default SSE stream echoing the model.
        let m = model.clone().unwrap_or_default();
        let chunks = vec![
            format!(
                "data: {{\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"{m}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"Hello\"}}}}]}}\n\n"
            ),
            format!(
                "data: {{\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"{m}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\" from mock\"}}}}]}}\n\n"
            ),
            "data: [DONE]\n\n".to_string(),
        ];
        return scripted(
            &state,
            &Script {
                chunks,
                base64: false,
                gate_after: None,
                content_type: None,
                status: None,
            },
        );
    }
    let m = model.unwrap_or_default();
    let resp = serde_json::json!({
        "id": "chatcmpl-mock", "object": "chat.completion", "model": m,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": format!("mock response from {m}") }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17 }
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        resp.to_string(),
    )
        .into_response()
}

fn scripted(state: &Arc<MockState>, script: &Script) -> Response {
    let status = StatusCode::from_u16(script.status.unwrap_or(200)).unwrap_or(StatusCode::OK);
    let ct = script
        .content_type
        .clone()
        .unwrap_or_else(|| "text/event-stream".into());
    let chunks: Vec<Bytes> = script
        .chunks
        .iter()
        .map(|c| {
            if script.base64 {
                Bytes::from(STANDARD.decode(c).unwrap_or_default())
            } else {
                Bytes::from(c.clone())
            }
        })
        .collect();
    let gate = script.gate_after;
    let st = Arc::clone(state);
    let stream = async_stream_chunks(st, chunks, gate);
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = status;
    let mut h = HeaderMap::new();
    if let Ok(v) = ct.parse() {
        h.insert(header::CONTENT_TYPE, v);
    }
    *resp.headers_mut() = h;
    resp
}

#[allow(clippy::needless_pass_by_value)]
fn async_stream_chunks(
    state: Arc<MockState>,
    chunks: Vec<Bytes>,
    gate: Option<usize>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        let mut guard = DisconnectGuard {
            state: Arc::clone(&state),
            finished: false,
        };
        for (i, c) in chunks.into_iter().enumerate() {
            if gate == Some(i) {
                while !state.released.load(Ordering::SeqCst) {
                    let notified = state.release.notified();
                    if state.released.load(Ordering::SeqCst) {
                        break;
                    }
                    notified.await;
                }
            }
            if tx.send(Ok(c)).await.is_err() {
                return; // client went away: guard records the disconnect
            }
            tokio::task::yield_now().await;
        }
        // For gated scripts, watch briefly whether the receiver goes away while we still
        // hold the sender: that only happens when the downstream connection was dropped.
        if gate.is_some() {
            tokio::select! {
                () = tx.closed() => return,
                () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
        }
        guard.finished = true;
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

async fn control_requests(State(state): State<Arc<MockState>>) -> Response {
    axum::Json(state.recorded().await).into_response()
}

async fn control_reset(State(state): State<Arc<MockState>>) -> Response {
    state.reset().await;
    StatusCode::NO_CONTENT.into_response()
}

async fn control_script(
    State(state): State<Arc<MockState>>,
    axum::Json(script): axum::Json<Script>,
) -> Response {
    state.set_script(script).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn control_release(State(state): State<Arc<MockState>>) -> Response {
    state.release();
    StatusCode::NO_CONTENT.into_response()
}

async fn control_stats(State(state): State<Arc<MockState>>) -> Response {
    axum::Json(serde_json::json!({ "requests": state.count.load(Ordering::SeqCst), "disconnects": state.disconnects.load(Ordering::SeqCst) })).into_response()
}

/// Build the router.
pub fn app(state: Arc<MockState>) -> Router {
    Router::new()
        .route(
            "/_control/requests",
            get(control_requests).delete(control_reset),
        )
        .route("/_control/script", post(control_script))
        .route("/_control/release", post(control_release))
        .route("/_control/stats", get(control_stats))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(any(handle))
        .with_state(state)
}

/// Spawn the mock on an ephemeral port (tests).
///
/// # Errors
/// On bind failure.
pub async fn spawn() -> anyhow::Result<(SocketAddr, Arc<MockState>)> {
    let state = Arc::new(MockState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = app(Arc::clone(&state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, state))
}
