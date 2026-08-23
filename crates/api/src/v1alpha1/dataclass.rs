// SPDX-License-Identifier: Apache-2.0
//! `DataClass`: a sensitivity level, its detection rules and tier constraints.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Capability, CommonStatus, Jurisdiction, OperatorControl, PiiEntity};

/// Spec of a [`DataClass`]. Higher `rank` is more sensitive; most restrictive wins.
#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "routed.io",
    version = "v1alpha1",
    kind = "DataClass",
    namespaced,
    status = "CommonStatus",
    shortname = "dc",
    category = "routed",
    printcolumn = r#"{"name":"Rank","type":"integer","jsonPath":".spec.rank"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct DataClassSpec {
    /// Sensitivity rank; higher is more sensitive.
    pub rank: u32,
    /// Human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How requests are mapped to this class.
    #[serde(default)]
    pub detection: Detection,
    /// Constraints imposed on candidate tiers when this class applies.
    #[serde(default)]
    pub constraints: DataClassConstraints,
}

/// Detection rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Detection {
    /// Values of `X-Routed-Data-Class` that map to this class.
    #[serde(default)]
    pub header_values: Vec<String>,
    /// PII entities whose presence infers this class.
    #[serde(default)]
    pub pii_entities: Vec<PiiEntity>,
    /// Minimum detector confidence for inference.
    #[serde(default = "default_min_confidence")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_confidence: f64,
}

impl Default for Detection {
    fn default() -> Self {
        Self {
            header_values: vec![],
            pii_entities: vec![],
            min_confidence: default_min_confidence(),
        }
    }
}

fn default_min_confidence() -> f64 {
    0.7
}

/// Constraints on tiers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct DataClassConstraints {
    /// Tier jurisdiction must be one of these (empty = any).
    #[serde(default)]
    pub require_jurisdiction: Vec<Jurisdiction>,
    /// Tiers exposed to the CLOUD Act are eliminated.
    #[serde(default)]
    pub forbid_cloud_act_exposed: bool,
    /// Tier operator control must be one of these (empty = any).
    #[serde(default)]
    pub require_operator_control: Vec<OperatorControl>,
    /// Tiers offering any of these capabilities are eliminated.
    #[serde(default)]
    pub forbid_capabilities: Vec<Capability>,
    /// Informational retention limit for gateway / downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retention_days: Option<u32>,
}
