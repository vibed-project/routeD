// SPDX-License-Identifier: Apache-2.0
//! Input, output and explanation types of the decision engine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

pub use routed_snapshot::{Complexity, PiiEntity, ReasoningLevel};

/// Untrusted request hints parsed from `X-Routed-*` headers.
///
/// By construction this type can only make a decision **more** restrictive:
/// a data class is merged by maximum rank, a policy override is honoured only
/// within the set of policies that already match the request, and dry-run
/// never changes the decision itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestHints {
    /// `X-Routed-Data-Class` values (all of them; merged by maximum rank).
    pub data_classes: Vec<String>,
    /// `X-Routed-Policy`.
    pub policy: Option<String>,
    /// `X-Routed-Dry-Run: true`.
    pub dry_run: bool,
}

/// Request context extracted by the ingress layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DecisionInput {
    /// `X-Routed-Tenant`.
    pub tenant: Option<String>,
    /// `X-Routed-Agent`.
    pub agent: Option<String>,
    /// Request path (for example `/v1/chat/completions`).
    pub path: String,
    /// `model` field of the request.
    pub requested_model: String,
    /// Whether the request carries tools / functions.
    pub tools_present: bool,
    /// Estimated input tokens.
    pub estimated_input_tokens: u64,
    /// Estimated output tokens (`max_tokens` or the profile default).
    pub estimated_output_tokens: u64,
    /// Untrusted hints.
    pub hints: RequestHints,
}

/// Classifier findings. Every field is optional; a missing field means the
/// corresponding classifier produced nothing, and `degraded` lists classifiers
/// that failed or timed out (which triggers the policy fallback).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Findings {
    /// Task label (for example `code`, `summarization`).
    pub task: Option<String>,
    /// Complexity.
    pub complexity: Option<Complexity>,
    /// Injection / risk score in `0..=1`.
    pub risk_score: Option<f64>,
    /// Detected PII entities (already thresholded by confidence).
    pub pii_entities: BTreeSet<PiiEntity>,
    /// Highest detector confidence per entity (absent = 1.0); data classes apply
    /// their own `minConfidence` against these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pii_confidence: BTreeMap<PiiEntity, f64>,
    /// Sensitivity label naming a `DataClass`.
    pub inferred_data_class: Option<String>,
    /// Classifiers that failed or timed out.
    pub degraded: Vec<String>,
}

/// Per-decision context supplied by the caller (injected for determinism).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContext {
    /// Decision id (ULID in production, fixed in tests).
    pub id: String,
}

/// Decision outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumIter)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    /// Rewrite the model to the selected tier.
    Route,
    /// Leave the request untouched.
    PassThrough,
    /// Reject the request.
    Block,
}

/// Reason a candidate was eliminated. Closed set; every variant has a golden example.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter)]
pub enum EliminationReason {
    /// Request blocked entirely by the policy risk threshold.
    #[strum(serialize = "hardConstraints.denyIfRiskScoreAbove")]
    DenyIfRiskScoreAbove,
    /// Tier does not list the data class in `allowedDataClasses`.
    #[strum(serialize = "dataClass.allowedDataClasses")]
    DataClassNotAllowed,
    /// Tier jurisdiction not allowed by the data class.
    #[strum(serialize = "dataClass.requireJurisdiction")]
    DataClassRequireJurisdiction,
    /// Tier is CLOUD Act exposed and the data class forbids it.
    #[strum(serialize = "dataClass.forbidCloudActExposed")]
    DataClassForbidCloudActExposed,
    /// Tier operator control not allowed by the data class.
    #[strum(serialize = "dataClass.requireOperatorControl")]
    DataClassRequireOperatorControl,
    /// Tier offers a capability the data class forbids.
    #[strum(serialize = "dataClass.forbidCapabilities")]
    DataClassForbidCapabilities,
    /// Tier lacks a capability the policy requires.
    #[strum(serialize = "hardConstraints.requireCapabilities")]
    RequireCapabilities,
    /// Request has tools but the tier has no tools capability.
    #[strum(serialize = "capabilities.tools")]
    ToolsNotSupported,
    /// Estimated tokens exceed the tier context window.
    #[strum(serialize = "capabilities.contextWindow")]
    ContextWindow,
    /// Risk score above the tier's `maxRiskScore`.
    #[strum(serialize = "security.maxRiskScore")]
    MaxRiskScore,
    /// Request has tools but the tier forbids tool calling.
    #[strum(serialize = "security.toolCallingAllowed")]
    ToolCallingNotAllowed,
    /// Estimated cost above the policy cap.
    #[strum(serialize = "hardConstraints.maxCostPerRequestEUR")]
    MaxCostPerRequest,
    /// Predicted quality below the policy floor.
    #[strum(serialize = "qualityFloor")]
    QualityFloor,
}

impl Serialize for EliminationReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for EliminationReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse()
            .map_err(|_| serde::de::Error::custom(format!("unknown elimination reason {s:?}")))
    }
}

impl EliminationReason {
    /// Whether this reason is a hard constraint (as opposed to the quality floor preference).
    #[must_use]
    pub fn is_hard(self) -> bool {
        !matches!(self, EliminationReason::QualityFloor)
    }
}

/// One candidate in the explanation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Tier name.
    pub tier: String,
    /// Why it was eliminated, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eliminated_by: Option<EliminationReason>,
    /// Predicted quality (survivors and quality-floor eliminations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_quality: Option<f64>,
    /// Estimated cost in EUR.
    #[serde(
        rename = "estimatedCostEUR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub estimated_cost_eur: Option<f64>,
    /// Final score (scored survivors only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Whether this candidate was selected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub selected: bool,
}

impl Candidate {
    pub(crate) fn eliminated(tier: &str, reason: EliminationReason) -> Self {
        Self {
            tier: tier.to_owned(),
            eliminated_by: Some(reason),
            predicted_quality: None,
            estimated_cost_eur: None,
            score: None,
            selected: false,
        }
    }
}

/// Parameters to inject into the request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameters {
    /// Reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    /// `max_tokens` cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// Classification summary in the explanation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    /// Task label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Complexity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<Complexity>,
    /// Risk score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<f64>,
    /// PII entities.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pii_entities: BTreeSet<PiiEntity>,
}

/// The decision. Identical JSON in `routedctl explain`, the
/// `X-Routed-Decision` header and the `routed.decision` span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// Decision id.
    pub id: String,
    /// Outcome.
    pub outcome: Outcome,
    /// Matched policy (`namespace/name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Model alias the caller asked for.
    pub requested_model: String,
    /// Selected tier (`ROUTE` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tier: Option<String>,
    /// Model name to send to the gateway (`ROUTE` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_model: Option<String>,
    /// Parameters to inject.
    #[serde(default)]
    pub parameters: Parameters,
    /// Effective data class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<String>,
    /// Classification summary.
    #[serde(default)]
    pub classification: Classification,
    /// Every candidate with its fate.
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// Estimated cost of the selected tier in EUR.
    #[serde(
        rename = "estimatedCostEUR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub estimated_cost_eur: Option<f64>,
    /// Savings versus the most expensive scored candidate in EUR.
    #[serde(
        rename = "estimatedSavingsEUR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub estimated_savings_eur: Option<f64>,
    /// Decision latency in ms (set by the caller).
    #[serde(default)]
    pub latency_ms: u64,
    /// Snapshot the decision was made against.
    pub snapshot_hash: String,
    /// Human-readable reason for `BLOCK`, `PASS_THROUGH` or fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether the fallback decision was applied.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// Classifiers that failed or timed out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<String>,
    /// Hints that were ignored and other notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Whether the request asked for a dry run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

impl Decision {
    pub(crate) fn new(
        ctx: &DecisionContext,
        input: &DecisionInput,
        findings: &Findings,
        snapshot_hash: &str,
    ) -> Self {
        Self {
            id: ctx.id.clone(),
            outcome: Outcome::PassThrough,
            policy: None,
            requested_model: input.requested_model.clone(),
            selected_tier: None,
            gateway_model: None,
            parameters: Parameters::default(),
            data_class: None,
            classification: Classification {
                task: findings.task.clone(),
                complexity: findings.complexity,
                risk_score: findings.risk_score.map(crate::round6),
                pii_entities: findings.pii_entities.clone(),
            },
            candidates: Vec::new(),
            estimated_cost_eur: None,
            estimated_savings_eur: None,
            latency_ms: 0,
            snapshot_hash: snapshot_hash.to_owned(),
            reason: None,
            fallback: false,
            degraded: findings.degraded.clone(),
            notes: Vec::new(),
            dry_run: input.hints.dry_run,
        }
    }

    /// Compact JSON.
    ///
    /// # Panics
    /// Never: the structure is plain data.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
