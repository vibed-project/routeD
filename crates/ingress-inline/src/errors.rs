// SPDX-License-Identifier: Apache-2.0
//! `OpenAI`-style error responses.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Build an `OpenAI`-format error response.
#[must_use]
pub fn error_response(
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    extra: Option<serde_json::Value>,
) -> Response {
    let mut error = json!({ "message": message, "type": error_type, "code": code, "param": null });
    if let Some(extra) = extra {
        if let (Some(obj), Some(e)) = (error.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let body = json!({ "error": error }).to_string();
    let mut resp = (status, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// 401/403 from the authenticator (ADR-0020). Any other status maps to 401.
#[must_use]
pub fn auth_denied(status: u16, reason: &str) -> Response {
    let (status, code) = match status {
        403 => (StatusCode::FORBIDDEN, "forbidden"),
        _ => (StatusCode::UNAUTHORIZED, "unauthorized"),
    };
    error_response(status, "invalid_request_error", code, reason, None)
}

/// 400 invalid request.
#[must_use]
pub fn bad_request(message: &str) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        "routed_invalid_request",
        message,
        None,
    )
}

/// 413 body too large.
#[must_use]
pub fn too_large(limit: usize) -> Response {
    error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        "invalid_request_error",
        "routed_body_too_large",
        &format!("request body exceeds {limit} bytes"),
        None,
    )
}

/// 503 not ready.
#[must_use]
pub fn not_ready() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "server_error",
        "routed_not_ready",
        "no routing snapshot loaded",
        None,
    )
}

/// 502 upstream failure.
#[must_use]
pub fn bad_gateway(message: &str) -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        "server_error",
        "routed_upstream_error",
        message,
        None,
    )
}
