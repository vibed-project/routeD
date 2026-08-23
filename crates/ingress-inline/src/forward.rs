// SPDX-License-Identifier: Apache-2.0
//! Upstream forwarding: header hygiene, trace propagation, streamed responses.

use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, Method, Request, Response, Uri, header};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use opentelemetry::global;
use opentelemetry::propagation::Injector;
use routed_security::is_routed_header;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::body::IdleTimeoutBody;

/// Upstream client with its base URL.
#[derive(Clone)]
pub struct Upstream {
    client: Client<HttpsConnector<HttpConnector>, Body>,
    scheme: String,
    authority: String,
    base_path: String,
}

/// Hop-by-hop headers never forwarded in either direction.
const HOP_BY_HOP: [HeaderName; 8] = [
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    HeaderName::from_static("keep-alive"),
];

/// Header names listed in `Connection:` (RFC 7230 hop-by-hop by declaration).
fn connection_listed(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| {
            v.split(',')
                .map(|s| s.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .filter_map(|s| HeaderName::from_bytes(s.as_bytes()).ok())
        .collect()
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(key.as_bytes()), value.parse()) {
            self.0.insert(k, v);
        }
    }
}

impl Upstream {
    /// Create from a base URL.
    ///
    /// # Errors
    /// When the URL has no scheme or host.
    pub fn new(
        client: Client<HttpsConnector<HttpConnector>, Body>,
        base: &str,
    ) -> anyhow::Result<Self> {
        let uri: Uri = base.parse()?;
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| anyhow::anyhow!("upstream URL needs a scheme"))?
            .to_owned();
        let authority = uri
            .authority()
            .ok_or_else(|| anyhow::anyhow!("upstream URL needs a host"))?
            .to_string();
        let base_path = uri.path().trim_end_matches('/').to_owned();
        Ok(Self {
            client,
            scheme,
            authority,
            base_path,
        })
    }

    /// Base URL string.
    #[must_use]
    pub fn base(&self) -> String {
        format!("{}://{}{}", self.scheme, self.authority, self.base_path)
    }

    fn target(&self, path_and_query: &str) -> anyhow::Result<Uri> {
        Ok(format!(
            "{}://{}{}{}",
            self.scheme, self.authority, self.base_path, path_and_query
        )
        .parse()?)
    }

    /// Forward a request (body may be streamed or buffered) and return the
    /// upstream response with the body passed through untouched apart from an
    /// idle watchdog. Inbound and upstream `x-routed-*` headers are dropped so
    /// neither side can spoof routeD's decision headers.
    ///
    /// # Errors
    /// On connection / protocol errors.
    pub async fn forward(
        &self,
        method: Method,
        path_and_query: &str,
        headers: &HeaderMap,
        body: Body,
        content_length: Option<u64>,
        idle: Duration,
    ) -> anyhow::Result<Response<Body>> {
        let uri = self.target(path_and_query)?;
        let mut builder = Request::builder().method(method).uri(uri);
        let out = builder
            .headers_mut()
            .ok_or_else(|| anyhow::anyhow!("request builder"))?;
        let listed = connection_listed(headers);
        for (name, value) in headers {
            if HOP_BY_HOP.contains(name)
                || listed.contains(name)
                || *name == header::HOST
                || *name == header::CONTENT_LENGTH
                || is_routed_header(name.as_str())
            {
                continue;
            }
            out.append(name.clone(), value.clone());
        }
        if let Some(len) = content_length {
            out.insert(header::CONTENT_LENGTH, len.into());
        }
        // W3C trace context propagation from the current span.
        let cx = tracing::Span::current().context();
        global::get_text_map_propagator(|p| p.inject_context(&cx, &mut HeaderInjector(out)));
        let req = builder.body(body)?;
        let resp = self.client.request(req).await?;
        let (mut parts, incoming) = resp.into_parts();
        let listed = connection_listed(&parts.headers);
        for h in HOP_BY_HOP.iter().chain(listed.iter()) {
            parts.headers.remove(h);
        }
        parts.headers.remove(header::HOST);
        let spoofed: Vec<HeaderName> = parts
            .headers
            .keys()
            .filter(|n| is_routed_header(n.as_str()))
            .cloned()
            .collect();
        for h in spoofed {
            parts.headers.remove(h);
        }
        let body = Body::new(IdleTimeoutBody::new(incoming, idle));
        Ok(Response::from_parts(parts, body))
    }
}
