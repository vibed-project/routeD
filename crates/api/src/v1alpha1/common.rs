// SPDX-License-Identifier: Apache-2.0
//! Enums and status types shared by several kinds.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Model capabilities a request may require and a tier may offer.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// Chat completions.
    Chat,
    /// Tool / function calling.
    Tools,
    /// JSON mode or structured outputs.
    Json,
    /// Image inputs.
    Vision,
    /// Audio inputs.
    Audio,
    /// Embeddings endpoint.
    Embeddings,
    /// Extended reasoning / thinking.
    Reasoning,
}

/// Legal jurisdiction a model operates under.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Jurisdiction {
    /// European Union.
    Eu,
    /// United States.
    Us,
    /// Any other jurisdiction.
    Other,
}

/// Who controls the operator of the model endpoint.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum OperatorControl {
    /// Operated by an EU legal entity.
    EuEntity,
    /// Operated by a US legal entity.
    UsEntity,
    /// Self-hosted / on-premises.
    OnPrem,
}

/// Currency of a price.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    /// Euro.
    #[default]
    Eur,
    /// US dollar.
    Usd,
}

/// Task complexity as produced by the classifier.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    /// Simple.
    Low,
    /// Moderate.
    Medium,
    /// Demanding.
    High,
}

/// Reasoning / thinking effort to request from the model.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    /// No extended reasoning.
    None,
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
}

/// PII entity categories detected by the security layer.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PiiEntity {
    /// A person's name.
    Person,
    /// Email address.
    Email,
    /// Phone number.
    Phone,
    /// Bank account number (IBAN).
    Iban,
    /// National identification number.
    NationalId,
    /// Health information.
    Health,
    /// Payment card number.
    CreditCard,
    /// Postal address.
    Address,
}

/// Type of a model / classifier artifact source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactType {
    /// ONNX model loaded in-process.
    #[default]
    Onnx,
    /// External HTTP service implementing the classifier contract.
    Http,
    /// Built-in heuristic implementation (no model).
    Heuristic,
    /// Deterministic stub returning configured labels (tests).
    Stub,
}

/// Status conditions plus optional observed generation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommonStatus {
    /// Standard Kubernetes conditions (`Ready`, `Compiled`, `ReferencesResolved`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Generation last processed by the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// Free-form string-to-string labels.
pub type Labels = BTreeMap<String, String>;
