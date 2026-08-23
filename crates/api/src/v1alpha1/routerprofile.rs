// SPDX-License-Identifier: Apache-2.0
//! `RouterProfile`: classifier, embedder and learned-router artifacts and calibration.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ArtifactType, CommonStatus};

/// Spec of a [`RouterProfile`].
#[derive(CustomResource, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "routed.io",
    version = "v1alpha1",
    kind = "RouterProfile",
    namespaced,
    status = "CommonStatus",
    shortname = "rprof",
    category = "routed",
    printcolumn = r#"{"name":"Classifier","type":"string","jsonPath":".spec.classifier.type"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct RouterProfileSpec {
    /// Multi-task classifier (task, complexity, sensitivity).
    #[serde(default)]
    pub classifier: ClassifierSpec,
    /// Embedding model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder: Option<EmbedderSpec>,
    /// Learned router model and calibration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_router: Option<LearnedRouterSpec>,
    /// Security models.
    #[serde(default)]
    pub security: SecuritySpec,
    /// Cost model parameters.
    #[serde(default)]
    pub cost_model: CostModel,
}

/// Classifier artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ClassifierSpec {
    /// Implementation type.
    #[serde(rename = "type", default)]
    pub type_: ArtifactType,
    /// Artifact location (`oci://...@sha256:...`, `https://...@sha256:...`
    /// or `file://...`, ADR-0016/0019) or HTTP endpoint for `type: http`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Tokenizer artifact (a Hugging Face `tokenizer.json`, same URI forms)
    /// required by `type: onnx` (ADR-0016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_uri: Option<String>,
    /// Label vocabularies.
    #[serde(default)]
    pub labels: ClassifierLabels,
    /// Per-call timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    #[schemars(range(min = 1))]
    pub timeout_ms: u64,
}

impl Default for ClassifierSpec {
    fn default() -> Self {
        Self {
            type_: ArtifactType::Heuristic,
            uri: None,
            tokenizer_uri: None,
            labels: ClassifierLabels::default(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

fn default_timeout_ms() -> u64 {
    25
}

/// Label vocabularies of the classifier.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ClassifierLabels {
    /// Task labels (match `ModelTier.spec.quality.byTask` keys).
    #[serde(default)]
    pub task: Vec<String>,
    /// Complexity labels.
    #[serde(default)]
    pub complexity: Vec<String>,
    /// Sensitivity labels (match `DataClass` names).
    #[serde(default)]
    pub sensitivity: Vec<String>,
}

/// Embedder artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EmbedderSpec {
    /// Implementation type.
    #[serde(rename = "type", default)]
    pub type_: ArtifactType,
    /// Artifact location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Embedding dimensions.
    #[schemars(range(min = 1))]
    pub dimensions: u32,
}

/// Learned router artifact and calibration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct LearnedRouterSpec {
    /// Implementation type.
    #[serde(rename = "type", default)]
    pub type_: ArtifactType,
    /// Artifact location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Quality floor (as string key) to probability threshold, produced by the trainer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub calibration: BTreeMap<String, f64>,
}

/// Security model artifacts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SecuritySpec {
    /// PII detector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii_detector: Option<ArtifactRef>,
    /// Injection detector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injection_detector: Option<ArtifactRef>,
}

/// Reference to a model artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Implementation type.
    #[serde(rename = "type", default)]
    pub type_: ArtifactType,
    /// Artifact location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// Cost model parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct CostModel {
    /// Conversion rates to EUR keyed by currency code (for example `USD: 0.92`).
    #[serde(
        rename = "fxToEUR",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub fx_to_eur: BTreeMap<String, f64>,
    /// Output tokens assumed when the request does not set `max_tokens`.
    #[serde(default = "default_output_tokens")]
    #[schemars(range(min = 1))]
    pub default_output_tokens: u64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            fx_to_eur: BTreeMap::new(),
            default_output_tokens: default_output_tokens(),
        }
    }
}

fn default_output_tokens() -> u64 {
    256
}
