// SPDX-License-Identifier: Apache-2.0
//! `ModelTier`: a model as the gateway exposes it plus ranking metadata.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Capability, CommonStatus, Currency, Jurisdiction, Labels, OperatorControl};

/// Spec of a [`ModelTier`]. routeD never calls the model; the gateway does.
#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "routed.io",
    version = "v1alpha1",
    kind = "ModelTier",
    namespaced,
    status = "CommonStatus",
    shortname = "mt",
    category = "routed",
    printcolumn = r#"{"name":"Gateway model","type":"string","jsonPath":".spec.gatewayModel"}"#,
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.provider"}"#,
    printcolumn = r#"{"name":"Jurisdiction","type":"string","jsonPath":".spec.sovereignty.jurisdiction"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ModelTierSpec {
    /// Exact model name the gateway expects in the `model` field.
    #[schemars(length(min = 1, max = 253))]
    pub gateway_model: String,
    /// Informational provider name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Capabilities this tier offers.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Maximum context window in tokens.
    #[schemars(range(min = 1))]
    pub context_window: u64,
    /// Prices.
    pub cost: Cost,
    /// Quality priors.
    #[serde(default)]
    pub quality: Quality,
    /// Latency priors.
    #[serde(default)]
    pub latency: Latency,
    /// Sovereignty attributes used by data-class constraints.
    pub sovereignty: Sovereignty,
    /// Security limits.
    #[serde(default)]
    pub security: TierSecurity,
    /// Free-form selector labels matched by `RoutingPolicy.spec.candidates.tierSelector`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: Labels,
}

/// Price per million tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Cost {
    /// Input price per million tokens.
    #[schemars(range(min = 0.0))]
    pub input_per_million: f64,
    /// Output price per million tokens.
    #[schemars(range(min = 0.0))]
    pub output_per_million: f64,
    /// Currency of both prices.
    #[serde(default)]
    pub currency: Currency,
}

/// Quality priors in `0..=1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Quality {
    /// Generic quality prior.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub baseline: f64,
    /// Per-task overrides keyed by task label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_task: BTreeMap<String, f64>,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            baseline: 0.5,
            by_task: BTreeMap::new(),
        }
    }
}

/// Latency priors in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Latency {
    /// Median latency.
    #[serde(default)]
    pub p50_ms: u64,
    /// 95th percentile latency.
    #[serde(default)]
    pub p95_ms: u64,
}

/// Sovereignty attributes of the model endpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Sovereignty {
    /// Legal jurisdiction.
    pub jurisdiction: Jurisdiction,
    /// Free-form region label (for example `eu-de`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_residency: Option<String>,
    /// Who operates the endpoint.
    pub operator_control: OperatorControl,
    /// Whether the operator is subject to the US CLOUD Act.
    #[serde(default)]
    pub cloud_act_exposed: bool,
    /// Data classes (by `DataClass` name) this tier may receive.
    #[serde(default)]
    pub allowed_data_classes: Vec<String>,
}

/// Security limits of a tier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TierSecurity {
    /// Highest injection / risk score this tier may receive (`0..=1`).
    #[serde(default = "default_max_risk")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub max_risk_score: f64,
    /// Whether requests containing tools may be routed here.
    #[serde(default = "default_true")]
    pub tool_calling_allowed: bool,
}

impl Default for TierSecurity {
    fn default() -> Self {
        Self {
            max_risk_score: default_max_risk(),
            tool_calling_allowed: true,
        }
    }
}

fn default_max_risk() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}
