// SPDX-License-Identifier: Apache-2.0
//! Immutable, content-hashed routing snapshot compiled from CRDs, plus the
//! hot-swappable holder used by the router.
//!
//! Pure data + atomic holder. No tokio, kube client, or network dependencies.
//! Everything in a [`Snapshot`] is ordered (`BTreeMap` / sorted `Vec`) so that
//! its canonical JSON, and therefore its hash, is deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use routed_api::v1alpha1::{
    ArtifactType, Capability, Complexity, Currency, Jurisdiction, ObjectiveMode, OperatorControl,
    PiiEntity, ReasoningLevel,
};

/// Snapshot schema version; bump on incompatible changes to the compiled form.
pub const SCHEMA_VERSION: u32 = 1;

/// Micro-EUR: one millionth of a euro. All money is integer in this unit.
pub type MicroEur = u64;

/// A compiled snapshot of all routing configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// `sha256:<hex>` over the canonical JSON of [`SnapshotCore`].
    pub hash: String,
    /// Everything the engine needs.
    #[serde(flatten)]
    pub core: SnapshotCore,
}

/// Hash-covered content of a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCore {
    /// Schema version of this structure.
    pub schema_version: u32,
    /// Compiler version that produced the snapshot.
    pub compiler_version: String,
    /// Tiers by name.
    pub tiers: BTreeMap<String, CompiledTier>,
    /// Data classes by name.
    pub data_classes: BTreeMap<String, CompiledDataClass>,
    /// Policies in evaluation order (priority desc, then namespace/name).
    pub policies: Vec<CompiledPolicy>,
    /// Router profiles by name.
    pub profiles: BTreeMap<String, CompiledProfile>,
}

impl Snapshot {
    /// Build a snapshot from its core, computing the hash.
    #[must_use]
    pub fn from_core(core: SnapshotCore) -> Self {
        let hash = core.content_hash();
        Self { hash, core }
    }

    /// Verify that `hash` matches the core content.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.hash == self.core.content_hash()
    }

    /// Look up a policy by `namespace/name` key.
    #[must_use]
    pub fn policy(&self, key: &str) -> Option<&CompiledPolicy> {
        self.core.policies.iter().find(|p| p.key == key)
    }

    /// Data class with the given name.
    #[must_use]
    pub fn data_class(&self, name: &str) -> Option<&CompiledDataClass> {
        self.core.data_classes.get(name)
    }

    /// Tier with the given name.
    #[must_use]
    pub fn tier(&self, name: &str) -> Option<&CompiledTier> {
        self.core.tiers.get(name)
    }

    /// Router profile for a policy: its `learnedRouter.profile`, else the profile
    /// named `default` in the policy's namespace, else the first profile.
    #[must_use]
    pub fn profile_for(&self, policy: &CompiledPolicy) -> Option<&CompiledProfile> {
        policy
            .learned_router
            .profile
            .as_deref()
            .and_then(|n| self.core.profiles.get(n))
            .or_else(|| self.core.profiles.get("default"))
            .or_else(|| self.core.profiles.values().next())
    }
}

impl SnapshotCore {
    /// Canonical JSON bytes (deterministic given the ordered containers).
    ///
    /// # Panics
    /// Never in practice: serialising plain data structures cannot fail.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `sha256:<hex>` of the canonical JSON.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let digest = Sha256::digest(self.canonical_json());
        format!("sha256:{}", hex::encode(digest))
    }
}

/// A compiled model tier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledTier {
    /// Tier name (`ModelTier.metadata.name`).
    pub name: String,
    /// Model name the gateway expects.
    pub gateway_model: String,
    /// Informational provider.
    pub provider: Option<String>,
    /// Offered capabilities.
    pub capabilities: BTreeSet<Capability>,
    /// Context window in tokens.
    pub context_window: u64,
    /// Input price in micro-EUR per million tokens.
    pub input_micro_eur_per_million: MicroEur,
    /// Output price in micro-EUR per million tokens.
    pub output_micro_eur_per_million: MicroEur,
    /// Original currency (informational; prices above are already EUR).
    pub currency: Currency,
    /// Generic quality prior.
    pub quality_baseline: f64,
    /// Per-task quality overrides.
    pub quality_by_task: BTreeMap<String, f64>,
    /// Median latency in ms.
    pub latency_p50_ms: u64,
    /// p95 latency in ms.
    pub latency_p95_ms: u64,
    /// Jurisdiction.
    pub jurisdiction: Jurisdiction,
    /// Region label.
    pub data_residency: Option<String>,
    /// Operator control.
    pub operator_control: OperatorControl,
    /// CLOUD Act exposure.
    pub cloud_act_exposed: bool,
    /// Data classes allowed on this tier.
    pub allowed_data_classes: BTreeSet<String>,
    /// Highest risk score accepted.
    pub max_risk_score: f64,
    /// Whether tool calling is allowed.
    pub tool_calling_allowed: bool,
    /// Selector labels.
    pub labels: BTreeMap<String, String>,
}

impl CompiledTier {
    /// Quality prior for a task label, falling back to the baseline.
    #[must_use]
    pub fn quality_for(&self, task: Option<&str>) -> f64 {
        task.and_then(|t| self.quality_by_task.get(t).copied())
            .unwrap_or(self.quality_baseline)
    }

    /// Estimated cost in micro-EUR for the given token counts.
    #[must_use]
    pub fn estimate_cost(&self, input_tokens: u64, output_tokens: u64) -> MicroEur {
        let input = u128::from(input_tokens) * u128::from(self.input_micro_eur_per_million);
        let output = u128::from(output_tokens) * u128::from(self.output_micro_eur_per_million);
        let total = (input + output).div_ceil(1_000_000);
        MicroEur::try_from(total).unwrap_or(MicroEur::MAX)
    }
}

/// A compiled data class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledDataClass {
    /// Name (`DataClass.metadata.name`).
    pub name: String,
    /// Sensitivity rank; higher is more sensitive.
    pub rank: u32,
    /// Header values mapping to this class (lowercased).
    pub header_values: BTreeSet<String>,
    /// PII entities inferring this class.
    pub pii_entities: BTreeSet<PiiEntity>,
    /// Minimum detector confidence.
    pub min_confidence: f64,
    /// Allowed jurisdictions (empty = any).
    pub require_jurisdiction: BTreeSet<Jurisdiction>,
    /// Eliminate CLOUD Act exposed tiers.
    pub forbid_cloud_act_exposed: bool,
    /// Allowed operator controls (empty = any).
    pub require_operator_control: BTreeSet<OperatorControl>,
    /// Forbidden capabilities.
    pub forbid_capabilities: BTreeSet<Capability>,
    /// Informational retention limit.
    pub max_retention_days: Option<u32>,
}

/// Normalised scoring weights (sum to 1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    /// Cost weight.
    pub cost: f64,
    /// Quality weight.
    pub quality: f64,
    /// Latency weight.
    pub latency: f64,
}

impl Weights {
    /// Default weights for an objective mode.
    #[must_use]
    pub fn for_mode(mode: ObjectiveMode) -> Self {
        match mode {
            ObjectiveMode::CostFirstWithQualityFloor => Self {
                cost: 0.6,
                quality: 0.3,
                latency: 0.1,
            },
            ObjectiveMode::QualityFirst => Self {
                cost: 0.1,
                quality: 0.8,
                latency: 0.1,
            },
            ObjectiveMode::Balanced => Self {
                cost: 1.0 / 3.0,
                quality: 1.0 / 3.0,
                latency: 1.0 / 3.0,
            },
            ObjectiveMode::LatencyFirst => Self {
                cost: 0.1,
                quality: 0.2,
                latency: 0.7,
            },
        }
    }

    /// Normalise so the weights sum to 1. All-zero weights yield the mode default.
    #[must_use]
    pub fn normalised(self, mode: ObjectiveMode) -> Self {
        let sum = self.cost + self.quality + self.latency;
        if sum <= 0.0 || !sum.is_finite() {
            return Self::for_mode(mode);
        }
        Self {
            cost: self.cost / sum,
            quality: self.quality / sum,
            latency: self.latency / sum,
        }
    }
}

/// Scope match of a compiled policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledMatch {
    /// Tenant patterns (empty = any).
    pub tenants: Vec<String>,
    /// Agent patterns (empty = any).
    pub agents: Vec<String>,
    /// Path patterns (empty = any).
    pub paths: Vec<String>,
    /// Requested-model alias patterns that trigger routing (empty = none; everything passes through).
    pub model_aliases: Vec<String>,
}

/// Learned router settings of a compiled policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledLearnedRouter {
    /// Enabled.
    pub enabled: bool,
    /// Profile name.
    pub profile: Option<String>,
    /// Confidence threshold.
    pub min_confidence: f64,
}

/// A compiled routing policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledPolicy {
    /// `namespace/name`.
    pub key: String,
    /// Namespace.
    pub namespace: String,
    /// Name.
    pub name: String,
    /// Priority (higher first).
    pub priority: i32,
    /// Scope.
    #[serde(rename = "match")]
    pub match_: CompiledMatch,
    /// Candidate tier names after selector / include / exclude resolution (sorted).
    pub candidates: Vec<String>,
    /// Apply data-class constraints.
    pub respect_data_class: bool,
    /// Cost cap per request in micro-EUR.
    pub max_cost_micro_eur: Option<MicroEur>,
    /// Capabilities every candidate must offer.
    pub require_capabilities: BTreeSet<Capability>,
    /// Block above this risk score.
    pub deny_if_risk_score_above: Option<f64>,
    /// Objective mode.
    pub mode: ObjectiveMode,
    /// Quality floor.
    pub quality_floor: Option<f64>,
    /// Normalised weights.
    pub weights: Weights,
    /// Learned router settings.
    pub learned_router: CompiledLearnedRouter,
    /// Reasoning budget enabled.
    pub reasoning_enabled: bool,
    /// Complexity to reasoning level.
    pub reasoning_map: BTreeMap<Complexity, ReasoningLevel>,
    /// Fallback tier.
    pub fallback_tier: Option<String>,
    /// Emit explanation.
    pub explain: bool,
    /// May be selected via `X-Routed-Policy` instead of the winning policy.
    pub overridable: bool,
}

/// A compiled router profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledProfile {
    /// Name.
    pub name: String,
    /// Classifier implementation type.
    pub classifier_type: ArtifactType,
    /// Classifier artifact / endpoint.
    pub classifier_uri: Option<String>,
    /// Classifier tokenizer artifact (`type: onnx`, ADR-0016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_tokenizer_uri: Option<String>,
    /// Classifier timeout in ms.
    pub classifier_timeout_ms: u64,
    /// Task labels.
    pub task_labels: Vec<String>,
    /// Sensitivity labels.
    pub sensitivity_labels: Vec<String>,
    /// Embedder artifact.
    pub embedder_uri: Option<String>,
    /// Embedding dimensions.
    pub embedder_dimensions: Option<u32>,
    /// Learned router artifact.
    pub learned_router_uri: Option<String>,
    /// Calibration: quality floor (string key) to probability cut.
    pub calibration: BTreeMap<String, f64>,
    /// PII detector artifact.
    pub pii_detector_uri: Option<String>,
    /// Injection detector artifact.
    pub injection_detector_uri: Option<String>,
    /// Output tokens assumed when the request does not set `max_tokens`.
    pub default_output_tokens: u64,
}

/// Hot-swappable holder for the current snapshot.
#[derive(Debug, Default)]
pub struct SnapshotHolder {
    current: ArcSwapOption<Snapshot>,
    previous: ArcSwapOption<Snapshot>,
    loaded_at: std::sync::Mutex<Option<std::time::Instant>>,
}

impl SnapshotHolder {
    /// Empty holder (router is not ready until [`SnapshotHolder::store`] is called).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Holder pre-populated with a snapshot.
    #[must_use]
    pub fn with(snapshot: Snapshot) -> Self {
        let holder = Self::new();
        holder.store(snapshot);
        holder
    }

    /// Current snapshot, if any. Callers load once per request and keep the `Arc`.
    #[must_use]
    pub fn load(&self) -> Option<Arc<Snapshot>> {
        self.current.load_full()
    }

    /// Atomically replace the current snapshot, retaining the previous generation.
    pub fn store(&self, snapshot: Snapshot) {
        let old = self.current.swap(Some(Arc::new(snapshot)));
        self.previous.store(old);
        if let Ok(mut g) = self.loaded_at.lock() {
            *g = Some(std::time::Instant::now());
        }
    }

    /// Time since the current snapshot was stored, if any.
    #[must_use]
    pub fn age(&self) -> Option<std::time::Duration> {
        self.loaded_at
            .lock()
            .ok()
            .and_then(|g| g.map(|t| t.elapsed()))
    }

    /// Revert to the previous generation, if one exists. Returns whether a revert happened.
    pub fn revert(&self) -> bool {
        match self.previous.swap(None) {
            Some(prev) => {
                self.current.store(Some(prev));
                true
            }
            None => false,
        }
    }

    /// Whether a snapshot is loaded.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.current.load().is_some()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn empty_core() -> SnapshotCore {
        SnapshotCore {
            schema_version: SCHEMA_VERSION,
            compiler_version: "test".into(),
            tiers: BTreeMap::new(),
            data_classes: BTreeMap::new(),
            policies: vec![],
            profiles: BTreeMap::new(),
        }
    }

    #[test]
    fn hash_is_stable_and_verifies() {
        let a = Snapshot::from_core(empty_core());
        let b = Snapshot::from_core(empty_core());
        assert_eq!(a.hash, b.hash);
        assert!(a.hash.starts_with("sha256:"));
        assert!(a.verify());
        let mut tampered = a.clone();
        tampered.core.compiler_version = "other".into();
        assert!(!tampered.verify());
    }

    #[test]
    fn cost_estimate_rounds_up() {
        let tier = CompiledTier {
            name: "t".into(),
            gateway_model: "m".into(),
            provider: None,
            capabilities: BTreeSet::new(),
            context_window: 1,
            input_micro_eur_per_million: 2_000_000, // 2 EUR / M tokens
            output_micro_eur_per_million: 6_000_000,
            currency: Currency::Eur,
            quality_baseline: 0.5,
            quality_by_task: BTreeMap::new(),
            latency_p50_ms: 0,
            latency_p95_ms: 0,
            jurisdiction: Jurisdiction::Eu,
            data_residency: None,
            operator_control: OperatorControl::EuEntity,
            cloud_act_exposed: false,
            allowed_data_classes: BTreeSet::new(),
            max_risk_score: 1.0,
            tool_calling_allowed: true,
            labels: BTreeMap::new(),
        };
        // 1000 in * 2 EUR/M = 0.002 EUR = 2000 micro; 500 out * 6 EUR/M = 0.003 EUR = 3000 micro
        assert_eq!(tier.estimate_cost(1000, 500), 5000);
        assert_eq!(tier.estimate_cost(1, 0), 2);
        assert_eq!(tier.quality_for(Some("none")), 0.5);
    }

    #[test]
    fn weights_normalise() {
        let w = Weights {
            cost: 2.0,
            quality: 1.0,
            latency: 1.0,
        }
        .normalised(ObjectiveMode::Balanced);
        assert!((w.cost - 0.5).abs() < 1e-12);
        let z = Weights {
            cost: 0.0,
            quality: 0.0,
            latency: 0.0,
        }
        .normalised(ObjectiveMode::LatencyFirst);
        assert_eq!(z, Weights::for_mode(ObjectiveMode::LatencyFirst));
    }

    #[test]
    fn holder_swaps_and_reverts() {
        let holder = SnapshotHolder::new();
        assert!(!holder.is_ready());
        holder.store(Snapshot::from_core(empty_core()));
        assert!(holder.is_ready());
        let mut c2 = empty_core();
        c2.compiler_version = "v2".into();
        holder.store(Snapshot::from_core(c2));
        assert_eq!(
            holder
                .load()
                .map(|s| s.core.compiler_version.clone())
                .as_deref(),
            Some("v2")
        );
        assert!(holder.revert());
        assert_eq!(
            holder
                .load()
                .map(|s| s.core.compiler_version.clone())
                .as_deref(),
            Some("test")
        );
        assert!(!holder.revert());
    }
}
