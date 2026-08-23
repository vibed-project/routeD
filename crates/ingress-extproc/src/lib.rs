// SPDX-License-Identifier: Apache-2.0
//! Envoy external processor (`ext_proc` v3) gRPC server (ADR-0017): the same
//! decision pipeline as the inline ingress, applied through Envoy mutations
//! instead of a forwarded request.
//!
//! Expected filter processing mode: request headers `SEND`, request body
//! `BUFFERED`, response headers `SEND`, response body `NONE`. Response bodies
//! never pass through routeD in this mode, so streaming integrity is Envoy's
//! job, not ours. `BLOCK` and dry-run become immediate responses; `ROUTE`
//! mutates the buffered request body; inbound `x-routed-*` headers are always
//! stripped before the request continues upstream (ADR-0007).

use std::sync::Arc;

use axum::response::Response;
use envoy_types::pb::envoy::config::core::v3::{
    HeaderMap as EnvoyHeaderMap, HeaderValue as EnvoyHeaderValue, HeaderValueOption,
    header_value_option::HeaderAppendAction,
};
use envoy_types::pb::envoy::extensions::filters::http::ext_proc::v3::ProcessingMode;
use envoy_types::pb::envoy::extensions::filters::http::ext_proc::v3::processing_mode::{
    BodySendMode, HeaderSendMode,
};
pub use envoy_types::pb::envoy::service::ext_proc::v3::external_processor_server::{
    ExternalProcessor, ExternalProcessorServer,
};
use envoy_types::pb::envoy::service::ext_proc::v3::processing_request::Request as Req;
use envoy_types::pb::envoy::service::ext_proc::v3::processing_response::Response as Resp;
use envoy_types::pb::envoy::service::ext_proc::v3::{
    BodyMutation, BodyResponse, CommonResponse, HeaderMutation, HeadersResponse, HttpBody,
    HttpHeaders, ImmediateResponse, ProcessingRequest, ProcessingResponse, TrailersResponse,
    body_mutation,
};
use envoy_types::pb::envoy::r#type::v3::HttpStatus;
use routed_decision::{Decision, Outcome};
use routed_ingress_inline::handlers::{
    ROUTED_PATHS, block_response, decide_bytes, normalize_path, rewrite_body,
};
use routed_ingress_inline::{AppState, headers as decision_headers};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Status, Streaming};

/// The `ext_proc` service. One gRPC stream corresponds to one HTTP request.
pub struct ExtProcService {
    state: Arc<AppState>,
}

impl ExtProcService {
    /// New service over the shared router state.
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl ExternalProcessor for ExtProcService {
    type ProcessStream = ReceiverStream<Result<ProcessingResponse, Status>>;

    async fn process(
        &self,
        request: Request<Streaming<ProcessingRequest>>,
    ) -> Result<tonic::Response<Self::ProcessStream>, Status> {
        let inbound = request.into_inner();
        let (tx, rx) = mpsc::channel(8);
        let state = Arc::clone(&self.state);
        tokio::spawn(stream_loop(state, inbound, tx));
        Ok(tonic::Response::new(ReceiverStream::new(rx)))
    }
}

/// What the stream has seen of the HTTP request so far.
struct Pending {
    path: String,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

async fn stream_loop(
    state: Arc<AppState>,
    mut inbound: Streaming<ProcessingRequest>,
    tx: mpsc::Sender<Result<ProcessingResponse, Status>>,
) {
    let mut pending: Option<Pending> = None;
    // Kept for the response-headers phase, mirroring the inline mode's
    // X-Routed-* response headers.
    let mut decided: Option<(Decision, bool)> = None;
    while let Some(msg) = inbound.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "ext_proc stream error");
                return;
            }
        };
        let response = match msg.request {
            Some(Req::RequestHeaders(h)) => {
                let (resp, p, dec) = on_request_headers(&state, &h).await;
                pending = p;
                if dec.is_some() {
                    decided = dec;
                }
                resp
            }
            Some(Req::RequestBody(b)) => match pending.as_mut() {
                Some(p) => match on_request_body(&state, p, &b).await {
                    Some((resp, dec)) => {
                        if dec.is_some() {
                            decided = dec;
                        }
                        resp
                    }
                    // Mid-stream chunk: acknowledge and keep buffering.
                    None => plain(Resp::RequestBody(BodyResponse::default())),
                },
                // Body for a request we chose not to process.
                None => plain(Resp::RequestBody(BodyResponse::default())),
            },
            Some(Req::ResponseHeaders(_)) => {
                let mutation = decided.as_ref().map(|(d, explain)| {
                    let mut hm = http::HeaderMap::new();
                    decision_headers::apply(&mut hm, d, *explain, state.config.decision_header_max);
                    HeaderMutation {
                        set_headers: to_set_headers(&hm),
                        remove_headers: Vec::new(),
                    }
                });
                plain(Resp::ResponseHeaders(HeadersResponse {
                    response: Some(CommonResponse {
                        header_mutation: mutation,
                        ..Default::default()
                    }),
                }))
            }
            Some(Req::ResponseBody(_)) => plain(Resp::ResponseBody(BodyResponse::default())),
            Some(Req::RequestTrailers(_)) => {
                plain(Resp::RequestTrailers(TrailersResponse::default()))
            }
            Some(Req::ResponseTrailers(_)) => {
                plain(Resp::ResponseTrailers(TrailersResponse::default()))
            }
            None => continue,
        };
        if tx.send(Ok(response)).await.is_err() {
            return;
        }
    }
}

/// Headers phase: route the stream. Returns the response, the pending state
/// for a routed request, and (for header-only routed requests) a decision.
async fn on_request_headers(
    state: &Arc<AppState>,
    h: &HttpHeaders,
) -> (
    ProcessingResponse,
    Option<Pending>,
    Option<(Decision, bool)>,
) {
    let (method, raw_path, headers) = split_envoy_headers(h.headers.as_ref());
    let strip = routed_header_names(&headers);
    let path = raw_path
        .split(['?', '#'])
        .next()
        .and_then(normalize_path)
        .unwrap_or_default();
    let routed = method == "POST" && ROUTED_PATHS.contains(&path.as_str());
    if !routed {
        // Not ours: strip x-routed-* and tell Envoy to skip the remaining
        // phases for this request (needs allow_mode_override: true).
        let resp = ProcessingResponse {
            mode_override: Some(ProcessingMode {
                request_body_mode: BodySendMode::None as i32,
                response_header_mode: HeaderSendMode::Skip as i32,
                ..Default::default()
            }),
            ..plain(Resp::RequestHeaders(HeadersResponse {
                response: Some(CommonResponse {
                    header_mutation: Some(HeaderMutation {
                        set_headers: Vec::new(),
                        remove_headers: strip,
                    }),
                    ..Default::default()
                }),
            }))
        };
        return (resp, None, None);
    }
    if h.end_of_stream {
        // A routed path with no body cannot parse; decide over empty bytes so
        // the caller gets the same 400 the inline mode produces.
        let mut p = Pending {
            path: path.clone(),
            headers: headers.clone(),
            body: Vec::new(),
        };
        let body = HttpBody {
            end_of_stream: true,
            ..Default::default()
        };
        if let Some((resp, dec)) = on_request_body(state, &mut p, &body).await {
            return (resp, None, dec);
        }
    }
    (
        plain(Resp::RequestHeaders(HeadersResponse::default())),
        Some(Pending {
            path,
            headers,
            body: Vec::new(),
        }),
        None,
    )
}

/// Body phase: buffer, and on the final chunk run the decision pipeline.
/// `None` means "interim chunk, keep buffering".
async fn on_request_body(
    state: &Arc<AppState>,
    p: &mut Pending,
    b: &HttpBody,
) -> Option<(ProcessingResponse, Option<(Decision, bool)>)> {
    if p.body.len().saturating_add(b.body.len()) > state.config.max_body_bytes {
        let resp = routed_ingress_inline::errors::too_large(state.config.max_body_bytes);
        return Some((immediate(resp).await, None));
    }
    p.body.extend_from_slice(&b.body);
    if !b.end_of_stream {
        return None;
    }
    let strip = routed_header_names(&p.headers);
    match decide_bytes(state, &p.path, &p.headers, &p.body, "extproc").await {
        Err(resp) => Some((immediate(resp).await, None)),
        Ok(d) => {
            let hmax = state.config.decision_header_max;
            if d.decision.dry_run {
                let mut resp = axum::response::IntoResponse::into_response((
                    http::StatusCode::OK,
                    [(http::header::CONTENT_TYPE, "application/json")],
                    d.decision.to_json(),
                ));
                decision_headers::apply(resp.headers_mut(), &d.decision, d.explain, hmax);
                return Some((immediate(resp).await, None));
            }
            match d.decision.outcome {
                Outcome::Block => {
                    let resp = block_response(&d.decision, d.explain, hmax);
                    Some((immediate(resp).await, None))
                }
                Outcome::Route | Outcome::PassThrough => {
                    // Envoy verifies content-length against a mutated buffered
                    // body, so a body replacement must update it in the same
                    // mutation.
                    let mut set_headers = Vec::new();
                    let body_mutation = if d.decision.outcome == Outcome::Route {
                        match rewrite_body(&p.path, &p.body, &d.decision) {
                            Ok(bytes) => {
                                set_headers.push(HeaderValueOption {
                                    header: Some(EnvoyHeaderValue {
                                        key: "content-length".to_owned(),
                                        value: String::new(),
                                        raw_value: bytes.len().to_string().into_bytes(),
                                    }),
                                    append_action: HeaderAppendAction::OverwriteIfExistsOrAdd
                                        as i32,
                                    keep_empty_value: false,
                                    ..Default::default()
                                });
                                Some(BodyMutation {
                                    mutation: Some(body_mutation::Mutation::Body(bytes)),
                                })
                            }
                            Err(e) => {
                                let resp = routed_ingress_inline::errors::bad_request(&format!(
                                    "cannot rewrite request: {e}"
                                ));
                                return Some((immediate(resp).await, None));
                            }
                        }
                    } else {
                        None
                    };
                    let resp = plain(Resp::RequestBody(BodyResponse {
                        response: Some(CommonResponse {
                            header_mutation: Some(HeaderMutation {
                                set_headers,
                                remove_headers: strip,
                            }),
                            body_mutation,
                            ..Default::default()
                        }),
                    }));
                    Some((resp, Some((d.decision, d.explain))))
                }
            }
        }
    }
}

fn plain(r: Resp) -> ProcessingResponse {
    ProcessingResponse {
        response: Some(r),
        ..Default::default()
    }
}

/// Inbound `x-routed-*` header names present on the request (ADR-0007: always
/// stripped before the request continues upstream).
fn routed_header_names(h: &http::HeaderMap) -> Vec<String> {
    h.keys()
        .filter(|k| routed_security::is_routed_header(k.as_str()))
        .map(|k| k.as_str().to_owned())
        .collect()
}

/// Convert a ready-made axum error / dry-run / block response into an Envoy
/// immediate response, preserving status, headers and body.
async fn immediate(resp: Response) -> ProcessingResponse {
    let status = resp.status();
    let mut set_headers = to_set_headers(resp.headers());
    // Envoy computes content-length for the immediate body itself.
    set_headers.retain(|h| {
        h.header
            .as_ref()
            .is_some_and(|hv| hv.key != "content-length")
    });
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    plain(Resp::ImmediateResponse(ImmediateResponse {
        status: Some(HttpStatus {
            code: i32::from(status.as_u16()),
        }),
        headers: Some(HeaderMutation {
            set_headers,
            remove_headers: Vec::new(),
        }),
        body: body.to_vec(),
        grpc_status: None,
        details: "routed_decision".to_owned(),
    }))
}

/// Envoy `HeaderMap` -> (`:method`, `:path`, http `HeaderMap` without pseudo headers).
#[allow(clippy::doc_markdown)]
fn split_envoy_headers(h: Option<&EnvoyHeaderMap>) -> (String, String, http::HeaderMap) {
    let mut method = String::new();
    let mut path = String::new();
    let mut out = http::HeaderMap::new();
    for hv in h.map(|m| m.headers.as_slice()).unwrap_or_default() {
        let value_bytes: &[u8] = if hv.raw_value.is_empty() {
            hv.value.as_bytes()
        } else {
            &hv.raw_value
        };
        match hv.key.as_str() {
            ":method" => method = String::from_utf8_lossy(value_bytes).into_owned(),
            ":path" => path = String::from_utf8_lossy(value_bytes).into_owned(),
            k if k.starts_with(':') => {}
            k => {
                if let (Ok(name), Ok(value)) = (
                    http::header::HeaderName::from_bytes(k.as_bytes()),
                    http::header::HeaderValue::from_bytes(value_bytes),
                ) {
                    out.append(name, value);
                }
            }
        }
    }
    (method, path, out)
}

/// http `HeaderMap` -> Envoy set-header mutations (overwrite semantics).
fn to_set_headers(h: &http::HeaderMap) -> Vec<HeaderValueOption> {
    h.iter()
        .map(|(k, v)| HeaderValueOption {
            header: Some(EnvoyHeaderValue {
                key: k.as_str().to_owned(),
                value: String::new(),
                raw_value: v.as_bytes().to_vec(),
            }),
            append_action: HeaderAppendAction::OverwriteIfExistsOrAdd as i32,
            keep_empty_value: false,
            ..Default::default()
        })
        .collect()
}

/// Serve the `ext_proc` gRPC service until `shutdown` resolves.
///
/// # Errors
/// On bind or serve errors.
pub async fn serve(
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    tracing::info!(%addr, "ext_proc ingress listening");
    tonic::transport::Server::builder()
        .add_service(ExternalProcessorServer::new(ExtProcService::new(state)))
        .serve_with_shutdown(addr, shutdown)
        .await?;
    Ok(())
}
