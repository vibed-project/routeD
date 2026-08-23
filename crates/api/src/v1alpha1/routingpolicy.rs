// SPDX-License-Identifier: Apache-2.0
//! `RoutingPolicy`: scope, candidates, hard constraints and objective.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Capability, Complexity, Labels, ReasoningLevel};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

/// Spec of a [`RoutingPolicy`].
#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "routed.io",
    version = "v1alpha1",
    kind = "RoutingPolicy",
    namespaced,
    status = "RoutingPolicyStatus",
    shortname = "rp",
    category = "routed",
    printcolumn = r#"{"name":"Priority","type":"integer","jsonPath":".spec.priority"}"#,
    printcolumn = r#"{"name":"Mode","type":"string","jsonPath":".spec.objective.mode"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct RoutingPolicySpec {
    /// Higher wins when several policies match.
    #[serde(default)]
    pub priority: i32,
    /// Scope of the policy.
    #[serde(rename = "match", default)]
    pub match_: PolicyMatch,
    /// Candidate tier selection.
    #[serde(default)]
    pub candidates: Candidates,
    /// Constraints that eliminate candidates (never weights).
    #[serde(default)]
    pub hard_constraints: HardConstraints,
    /// Optimisation objective over surviving candidates.
    #[serde(default)]
    pub objective: Objective,
    /// Learned router settings.
    #[serde(default)]
    pub learned_router: LearnedRouterSettings,
    /// Reasoning budget per complexity.
    #[serde(default)]
    pub reasoning_budget: ReasoningBudget,
    /// Decision applied when classification fails or no candidate survives.
    #[serde(default)]
    pub fallback_decision: FallbackDecision,
    /// Emit the explanation header.
    #[serde(default = "default_true")]
    pub explain: bool,
    /// Allow callers to select this policy with `X-Routed-Policy` when it matches the
    /// request but a higher-priority policy would otherwise win. Off by default so
    /// an untrusted header can never move a request to a looser policy (ADR-0007).
    #[serde(default)]
    pub overridable: bool,
}

/// Status of a [`RoutingPolicy`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutingPolicyStatus {
    /// Standard conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Hash of the snapshot this policy was last compiled into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_hash: Option<String>,
    /// Generation last processed by the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// Scope match. Empty lists match everything; `*` and `prefix/*` globs are supported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct PolicyMatch {
    /// Tenants from `X-Routed-Tenant`.
    #[serde(default)]
    pub tenants: Vec<String>,
    /// Agents from `X-Routed-Agent`.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Request paths.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Requested model aliases that trigger routing; anything else passes through.
    #[serde(default)]
    pub model_aliases: Vec<String>,
}

/// Candidate selection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Candidates {
    /// Label selector over `ModelTier.spec.labels`.
    #[serde(default)]
    pub tier_selector: TierSelector,
    /// Explicit tier names to include.
    #[serde(default)]
    pub include: Vec<String>,
    /// Tier names to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Label selector.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TierSelector {
    /// All labels must match.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub match_labels: Labels,
}

/// Hard constraints.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct HardConstraints {
    /// Apply `DataClass` constraints.
    #[serde(default = "default_true")]
    pub respect_data_class: bool,
    /// Maximum estimated cost per request in EUR.
    #[serde(
        rename = "maxCostPerRequestEUR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(range(min = 0.0))]
    pub max_cost_per_request_eur: Option<f64>,
    /// Capabilities every candidate must offer.
    #[serde(default)]
    pub require_capabilities: Vec<Capability>,
    /// Block the request entirely above this risk score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub deny_if_risk_score_above: Option<f64>,
}

impl Default for HardConstraints {
    fn default() -> Self {
        Self {
            respect_data_class: true,
            max_cost_per_request_eur: None,
            require_capabilities: vec![],
            deny_if_risk_score_above: None,
        }
    }
}

/// Objective mode.
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
#[serde(rename_all = "kebab-case")]
pub enum ObjectiveMode {
    /// Cheapest candidate above the quality floor.
    #[default]
    CostFirstWithQualityFloor,
    /// Highest predicted quality, cost as tie-break.
    QualityFirst,
    /// Equal weights.
    Balanced,
    /// Lowest latency, then quality.
    LatencyFirst,
}

/// Objective.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Objective {
    /// Mode.
    #[serde(default)]
    pub mode: ObjectiveMode,
    /// Minimum predicted quality for the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub quality_floor: Option<f64>,
    /// Explicit weights; defaults depend on `mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<Weights>,
}

/// Scoring weights; normalised by the compiler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Weights {
    /// Cost weight.
    #[schemars(range(min = 0.0))]
    pub cost: f64,
    /// Quality weight.
    #[schemars(range(min = 0.0))]
    pub quality: f64,
    /// Latency weight.
    #[schemars(range(min = 0.0))]
    pub latency: f64,
}

/// Learned router settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct LearnedRouterSettings {
    /// Use the learned router for quality prediction.
    #[serde(default)]
    pub enabled: bool,
    /// `RouterProfile` name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Below this confidence, fall back to heuristic ranking.
    #[serde(default = "default_min_confidence")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_confidence: f64,
}

impl Default for LearnedRouterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: None,
            min_confidence: default_min_confidence(),
        }
    }
}

fn default_min_confidence() -> f64 {
    0.6
}

/// Reasoning budget mapping.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ReasoningBudget {
    /// Set reasoning parameters per complexity.
    #[serde(default)]
    pub enabled: bool,
    /// Complexity to reasoning level.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub map: BTreeMap<Complexity, ReasoningLevel>,
}

/// Fallback decision.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct FallbackDecision {
    /// Tier used when classification fails or times out, if it satisfies the data class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

fn default_true() -> bool {
    true
}
