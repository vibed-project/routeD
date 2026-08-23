// SPDX-License-Identifier: Apache-2.0
//! Validating admission webhook (ADR-0015): rejects CRD writes that would
//! fail compilation, using the same `routed-policy` compiler as the
//! reconciler and `routedctl validate` (ADR-0008).
//!
//! Validation runs against current cluster state with the incoming object
//! substituted; only diagnostics attributed to the incoming object deny it,
//! so a write is never rejected because some other object is broken.
//! Failures to read cluster state fail open (the async status conditions
//! remain the safety net), matching the chart's default
//! `failurePolicy: Ignore`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use kube::Client;
use kube::core::DynamicObject;
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation};
use routed_policy::{CompileError, CompileInput, CompileReport, Level};

/// Everything a validation needs.
pub struct WebhookState {
    /// Cluster access for listing current objects.
    pub client: Client,
    /// Same namespace restriction as the reconciler.
    pub watch_namespace: Option<String>,
}

/// `POST /validate`.
pub fn router(state: Arc<WebhookState>) -> Router {
    Router::new()
        .route("/validate", post(validate))
        .with_state(state)
}

async fn validate(
    State(state): State<Arc<WebhookState>>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    let req: AdmissionRequest<DynamicObject> = match review.try_into() {
        Ok(r) => r,
        Err(e) => return Json(AdmissionResponse::invalid(e.to_string()).into_review()),
    };
    let resp = AdmissionResponse::from(&req);
    Json(decide(&state, &req, resp).await.into_review())
}

async fn decide(
    state: &WebhookState,
    req: &AdmissionRequest<DynamicObject>,
    resp: AdmissionResponse,
) -> AdmissionResponse {
    if !matches!(req.operation, Operation::Create | Operation::Update) {
        return resp;
    }
    let Some(obj) = &req.object else { return resp };
    let kind = req.kind.kind.clone();
    if !matches!(
        kind.as_str(),
        "ModelTier" | "DataClass" | "RoutingPolicy" | "RouterProfile"
    ) {
        return resp;
    }
    let mut input =
        match crate::reconcile::list_input(&state.client, state.watch_namespace.as_deref()).await {
            Ok(i) => i,
            Err(e) => {
                // Fail open: an unreadable cluster must not block CRD writes; the
                // reconciler's status conditions will still flag a bad object.
                tracing::warn!(error = %e, "webhook could not list cluster state; allowing");
                return resp;
            }
        };
    let value = match serde_json::to_value(obj) {
        Ok(v) => v,
        Err(e) => return resp.deny(format!("unreadable object: {e}")),
    };
    let key = match substitute(&mut input, &kind, value) {
        Ok(key) => key,
        Err(e) => return resp.deny(format!("not a valid {kind}: {e}")),
    };
    match check(&input, &kind, &key) {
        Ok(warnings) if warnings.is_empty() => resp,
        Ok(warnings) => {
            let mut resp = resp;
            resp.warnings = Some(warnings);
            resp
        }
        Err(denial) => resp.deny(denial),
    }
}

/// `namespace/name` as `routed_policy` names it in diagnostics.
fn object_key(ns: Option<&str>, name: Option<&str>) -> String {
    format!(
        "{}/{}",
        ns.unwrap_or("default"),
        name.unwrap_or("<unnamed>")
    )
}

/// Deserialize the incoming object as `kind` and replace (or append) it in
/// the compiler input, returning its diagnostic key.
fn substitute(
    input: &mut CompileInput,
    kind: &str,
    value: serde_json::Value,
) -> Result<String, String> {
    fn put<T, F>(list: &mut Vec<T>, value: serde_json::Value, meta: F) -> Result<String, String>
    where
        T: serde::de::DeserializeOwned,
        F: Fn(&T) -> (Option<&str>, Option<&str>),
    {
        let obj: T = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let (ns, name) = meta(&obj);
        let key = object_key(ns, name);
        let same = |t: &T| {
            let (tns, tname) = meta(t);
            object_key(tns, tname) == key
        };
        if let Some(slot) = list.iter_mut().find(|t| same(t)) {
            *slot = obj;
        } else {
            list.push(obj);
        }
        Ok(key)
    }
    match kind {
        "ModelTier" => put(&mut input.tiers, value, |o| {
            (o.metadata.namespace.as_deref(), o.metadata.name.as_deref())
        }),
        "DataClass" => put(&mut input.data_classes, value, |o| {
            (o.metadata.namespace.as_deref(), o.metadata.name.as_deref())
        }),
        "RoutingPolicy" => put(&mut input.policies, value, |o| {
            (o.metadata.namespace.as_deref(), o.metadata.name.as_deref())
        }),
        "RouterProfile" => put(&mut input.profiles, value, |o| {
            (o.metadata.namespace.as_deref(), o.metadata.name.as_deref())
        }),
        other => Err(format!("unsupported kind {other}")),
    }
}

/// Compile and attribute: deny only on error diagnostics naming the incoming
/// object; return its warnings otherwise.
fn check(input: &CompileInput, kind: &str, key: &str) -> Result<Vec<String>, String> {
    let report: CompileReport = match routed_policy::compile(input) {
        Ok((_, report)) | Err(CompileError(report)) => report,
    };
    let mine: Vec<_> = report
        .diags
        .iter()
        .filter(|d| d.kind == kind && d.name == key)
        .collect();
    let errors: Vec<String> = mine
        .iter()
        .filter(|d| d.level == Level::Error)
        .map(|d| format!("{}: {}", d.field, d.message))
        .collect();
    if errors.is_empty() {
        Ok(mine
            .iter()
            .filter(|d| d.level == Level::Warning)
            .map(|d| format!("{}: {}", d.field, d.message))
            .collect())
    } else {
        Err(errors.join("; "))
    }
}

/// Serve the webhook over TLS until the process exits.
///
/// # Errors
/// On unreadable certificates or a failed bind; connection-level errors are
/// logged and do not end the server.
pub async fn serve(
    addr: SocketAddr,
    certs_dir: PathBuf,
    state: Arc<WebhookState>,
) -> anyhow::Result<()> {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs =
        CertificateDer::pem_file_iter(certs_dir.join("tls.crt"))?.collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(certs_dir.join("tls.key"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = router(state);
    tracing::info!(%addr, "admission webhook serving");
    loop {
        let (stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let svc = hyper_util::service::TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            match acceptor.accept(stream).await {
                Ok(tls) => {
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                        .await
                    {
                        tracing::debug!(%peer, error = %e, "webhook connection error");
                    }
                }
                Err(e) => tracing::debug!(%peer, error = %e, "webhook TLS handshake failed"),
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use routed_policy::load::{into_input, parse_documents};

    fn example_input() -> CompileInput {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/001-route-cost-first-basic/resources.yaml");
        let text = std::fs::read_to_string(p).unwrap();
        into_input(parse_documents(&text).unwrap())
    }

    fn policy(name: &str, include: &str) -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "routed.io/v1alpha1",
            "kind": "RoutingPolicy",
            "metadata": { "name": name, "namespace": "ai-platform" },
            "spec": {
                "match": { "modelAliases": ["auto"] },
                "candidates": { "include": [include] }
            }
        })
    }

    #[test]
    fn broken_policy_is_denied_with_the_compiler_diagnostic() {
        let mut input = example_input();
        let key = substitute(
            &mut input,
            "RoutingPolicy",
            policy("e2e-invalid", "no-such-tier"),
        )
        .unwrap();
        assert_eq!(key, "ai-platform/e2e-invalid");
        let denial = check(&input, "RoutingPolicy", &key).unwrap_err();
        assert!(denial.contains("no-such-tier"), "{denial}");
    }

    #[test]
    fn valid_policy_passes_even_if_another_object_is_broken() {
        let mut input = example_input();
        // Break a different policy first; the incoming valid one must still pass.
        substitute(
            &mut input,
            "RoutingPolicy",
            policy("someone-elses-mess", "no-such-tier"),
        )
        .unwrap();
        let key = substitute(
            &mut input,
            "RoutingPolicy",
            policy("fine", "eu-sovereign-small"),
        )
        .unwrap();
        assert!(check(&input, "RoutingPolicy", &key).is_ok());
    }

    #[test]
    fn update_replaces_the_stored_object_instead_of_duplicating_it() {
        let mut input = example_input();
        let before = input.policies.len();
        substitute(
            &mut input,
            "RoutingPolicy",
            policy("default-cost-secure", "eu-sovereign-small"),
        )
        .unwrap();
        assert_eq!(input.policies.len(), before, "existing object replaced");
    }

    #[test]
    fn malformed_object_is_rejected() {
        let mut input = example_input();
        let err = substitute(
            &mut input,
            "RoutingPolicy",
            serde_json::json!({ "metadata": {}, "spec": { "priority": "not-a-number" } }),
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }
}
