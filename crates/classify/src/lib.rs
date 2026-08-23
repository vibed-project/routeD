// SPDX-License-Identifier: Apache-2.0
//! Classifier interfaces and implementations producing task, complexity,
//! sensitivity and risk findings.
//!
//! The trait is synchronous and CPU-bound by design: the router runs
//! classifiers on a bounded blocking pool under a strict timeout and maps
//! timeouts to [`routed_decision::Findings::degraded`], which makes the engine
//! apply the policy fallback (ADR-0006). ONNX implementations live behind the
//! `onnx` cargo feature (phase 4); `type: http` arrives with the ingress work.

pub mod conformance;
pub mod heuristic;
pub mod http;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod router_features;
pub mod stub;

use routed_decision::Findings;
use routed_snapshot::{ArtifactType, CompiledProfile};

pub use heuristic::HeuristicClassifier;
pub use http::HttpClassifier;
#[cfg(feature = "onnx")]
pub use onnx::OnnxClassifier;
pub use stub::StubClassifier;

/// Text selected for classification: the last user message, a truncated
/// system prompt, and any tool outputs (the main injection vector).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassifyInput {
    /// Truncated system prompt.
    pub system_prompt: Option<String>,
    /// Last user message (or the whole prompt for completions).
    pub user_text: String,
    /// Earlier user turns: scanned for PII (they are forwarded too) but not for injection.
    pub history: Vec<String>,
    /// Tool / function results present in the conversation.
    pub tool_outputs: Vec<String>,
}

impl ClassifyInput {
    /// Input with only a user message.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            user_text: text.into(),
            ..Default::default()
        }
    }
}

/// Classifier failure. Every variant makes the engine apply the fallback decision.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClassifyError {
    /// The per-classifier deadline elapsed.
    #[error("classifier {0} timed out")]
    Timeout(String),
    /// The configured implementation is not available in this build / environment.
    #[error("classifier unavailable: {0}")]
    Unavailable(String),
    /// The classifier ran and failed.
    #[error("classifier failed: {0}")]
    Failed(String),
}

/// A classifier. Implementations must be cheap to call concurrently.
pub trait Classifier: Send + Sync {
    /// Stable name used in telemetry and `Findings::degraded`.
    fn name(&self) -> &'static str;
    /// Classify. Must never panic; on failure return an error rather than a guess.
    ///
    /// # Errors
    /// See [`ClassifyError`].
    fn classify(&self, input: &ClassifyInput) -> Result<Findings, ClassifyError>;
}

/// Build the learned-router quality predictor configured by a profile
/// (ADR-0018): `Ok(None)` when the profile has no `learnedRouter.uri`.
///
/// # Errors
/// `Unavailable` when the artifact cannot be resolved or this build lacks
/// the `onnx` feature.
pub fn predictor_from_profile(
    profile: Option<&CompiledProfile>,
) -> Result<Option<routed_decision::SharedPredictor>, ClassifyError> {
    let Some(p) = profile else { return Ok(None) };
    let Some(uri) = p.learned_router_uri.clone() else {
        return Ok(None);
    };
    #[cfg(not(feature = "onnx"))]
    {
        let _ = uri;
        Err(ClassifyError::Unavailable(
            "learnedRouter.uri is set but this build has no ONNX support (feature `onnx`)".into(),
        ))
    }
    #[cfg(feature = "onnx")]
    {
        let resolver = routed_artifact::Resolver::from_env();
        let model = resolver
            .resolve(&uri)
            .map_err(|e| ClassifyError::Unavailable(e.to_string()))?;
        let predictor = onnx::OnnxQualityPredictor::load(&model, p.task_labels.clone())?;
        Ok(Some(std::sync::Arc::new(predictor)))
    }
}

/// Build the classifier configured by a profile (heuristic when absent).
///
/// # Errors
/// `Unavailable` for implementation types not compiled into this binary.
pub fn from_profile(
    profile: Option<&CompiledProfile>,
) -> Result<Box<dyn Classifier>, ClassifyError> {
    match profile.map_or(ArtifactType::Heuristic, |p| p.classifier_type) {
        ArtifactType::Heuristic => Ok(Box::new(HeuristicClassifier::default())),
        ArtifactType::Stub => Ok(Box::new(StubClassifier::default())),
        ArtifactType::Http => {
            let p = profile.ok_or_else(|| {
                ClassifyError::Unavailable("http classifier needs a profile".into())
            })?;
            let endpoint = p
                .classifier_uri
                .clone()
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .ok_or_else(|| {
                    ClassifyError::Unavailable("type: http requires an http(s):// uri".into())
                })?;
            Ok(Box::new(HttpClassifier {
                endpoint,
                timeout: std::time::Duration::from_millis(p.classifier_timeout_ms),
                bearer: std::env::var("ROUTED_CLASSIFIER_TOKEN").ok(),
            }))
        }
        #[cfg(not(feature = "onnx"))]
        ArtifactType::Onnx => Err(ClassifyError::Unavailable(
            "this build has no ONNX support (feature `onnx`)".into(),
        )),
        #[cfg(feature = "onnx")]
        ArtifactType::Onnx => {
            let p = profile.ok_or_else(|| {
                ClassifyError::Unavailable("onnx classifier needs a profile".into())
            })?;
            let model_uri = p.classifier_uri.clone().ok_or_else(|| {
                ClassifyError::Unavailable("type: onnx requires spec.classifier.uri".into())
            })?;
            let tokenizer_uri = p.classifier_tokenizer_uri.clone().ok_or_else(|| {
                ClassifyError::Unavailable(
                    "type: onnx requires spec.classifier.tokenizerUri".into(),
                )
            })?;
            let resolver = routed_artifact::Resolver::from_env();
            let unavailable =
                |e: routed_artifact::FetchError| ClassifyError::Unavailable(e.to_string());
            let model = resolver.resolve(&model_uri).map_err(unavailable)?;
            let tokenizer = resolver.resolve(&tokenizer_uri).map_err(unavailable)?;
            Ok(Box::new(onnx::OnnxClassifier::load(
                &model,
                &tokenizer,
                p.task_labels.clone(),
                p.sensitivity_labels.clone(),
            )?))
        }
    }
}
