// SPDX-License-Identifier: Apache-2.0
//! Policy compiler: validates and cross-references CRDs and produces a
//! [`routed_snapshot::Snapshot`]. Shared by the operator, the admission
//! webhook (dry-run), `routedctl validate`, and tests.
//!
//! Pure and deterministic: the same inputs always yield the same snapshot
//! hash. No I/O crates. YAML parsing lives in [`load`]; reading files is the
//! caller's job.

pub mod load;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use routed_api::v1alpha1::{
    Currency, DataClass, DataClassSpec, ModelTier, ModelTierSpec, RouterProfile, RoutingPolicy,
    RoutingPolicySpec,
};
use routed_snapshot::{
    CompiledDataClass, CompiledLearnedRouter, CompiledMatch, CompiledPolicy, CompiledProfile,
    CompiledTier, MicroEur, SCHEMA_VERSION, Snapshot, SnapshotCore, Weights,
};
use serde::{Deserialize, Serialize};

/// Version of this compiler, embedded in every snapshot.
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything the compiler consumes.
#[derive(Clone, Debug, Default)]
pub struct CompileInput {
    /// Model tiers.
    pub tiers: Vec<ModelTier>,
    /// Data classes.
    pub data_classes: Vec<DataClass>,
    /// Routing policies.
    pub policies: Vec<RoutingPolicy>,
    /// Router profiles.
    pub profiles: Vec<RouterProfile>,
}

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Informational warning; compilation succeeds.
    Warning,
    /// Compilation fails.
    Error,
}

/// One diagnostic, printed identically by the webhook and `routedctl validate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diag {
    /// Severity.
    pub level: Level,
    /// Resource kind.
    pub kind: String,
    /// `namespace/name`.
    pub name: String,
    /// JSON-path-ish field reference.
    pub field: String,
    /// Human message.
    pub message: String,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.level {
            Level::Warning => "warning",
            Level::Error => "error",
        };
        write!(
            f,
            "{level}: {} {} {}: {}",
            self.kind, self.name, self.field, self.message
        )
    }
}

/// All diagnostics of one compilation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileReport {
    /// Diagnostics in emission order.
    pub diags: Vec<Diag>,
}

impl CompileReport {
    /// Whether any error was emitted.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.level == Level::Error)
    }

    /// Errors only.
    pub fn errors(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Error)
    }

    /// Warnings only.
    pub fn warnings(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Warning)
    }

    fn push(
        &mut self,
        level: Level,
        kind: &str,
        name: &str,
        field: &str,
        message: impl Into<String>,
    ) {
        self.diags.push(Diag {
            level,
            kind: kind.into(),
            name: name.into(),
            field: field.into(),
            message: message.into(),
        });
    }

    fn error(&mut self, kind: &str, name: &str, field: &str, message: impl Into<String>) {
        self.push(Level::Error, kind, name, field, message);
    }

    fn warn(&mut self, kind: &str, name: &str, field: &str, message: impl Into<String>) {
        self.push(Level::Warning, kind, name, field, message);
    }
}

impl fmt::Display for CompileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.diags {
            writeln!(f, "{d}")?;
        }
        Ok(())
    }
}

/// Compilation failed; the report carries at least one error.
#[derive(Debug, thiserror::Error)]
#[error("compilation failed with {} error(s)", .0.errors().count())]
pub struct CompileError(pub CompileReport);

fn ns_name(ns: Option<&str>, name: Option<&str>) -> String {
    format!(
        "{}/{}",
        ns.unwrap_or("default"),
        name.unwrap_or("<unnamed>")
    )
}

fn within(v: f64, lo: f64, hi: f64) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}

/// Convert a price to micro-EUR per million tokens.
fn to_micro_eur(price: f64, currency: Currency, fx: &BTreeMap<String, f64>) -> Option<MicroEur> {
    let rate = match currency {
        Currency::Eur => 1.0,
        Currency::Usd => *fx.get("USD")?,
    };
    let micro = (price * rate * 1_000_000.0).round();
    if !micro.is_finite() || micro < 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(micro as MicroEur)
}

/// Compile CRDs into a snapshot. Warnings are returned alongside a successful
/// snapshot; any error fails compilation.
///
/// # Errors
/// Returns the full report when at least one error diagnostic was emitted.
pub fn compile(input: &CompileInput) -> Result<(Snapshot, CompileReport), CompileError> {
    let mut report = CompileReport::default();

    // ---- FX table: union of all profiles; conflicting rates are an error ----
    let mut fx: BTreeMap<String, f64> = BTreeMap::new();
    for p in &input.profiles {
        let pname = ns_name(p.metadata.namespace.as_deref(), p.metadata.name.as_deref());
        for (cur, rate) in &p.spec.cost_model.fx_to_eur {
            if !within(*rate, 0.0, f64::MAX) || *rate == 0.0 {
                report.error(
                    "RouterProfile",
                    &pname,
                    &format!("spec.costModel.fxToEUR.{cur}"),
                    "rate must be a positive number",
                );
                continue;
            }
            match fx.get(cur) {
                Some(existing) if (existing - rate).abs() > f64::EPSILON => {
                    report.error(
                        "RouterProfile",
                        &pname,
                        &format!("spec.costModel.fxToEUR.{cur}"),
                        format!("conflicting rate {rate} (another profile declares {existing})"),
                    );
                }
                _ => {
                    fx.insert(cur.clone(), *rate);
                }
            }
        }
    }

    // ---- profiles ----
    let mut profiles = BTreeMap::new();
    for p in &input.profiles {
        let pname = ns_name(p.metadata.namespace.as_deref(), p.metadata.name.as_deref());
        let Some(name) = p.metadata.name.clone() else {
            report.error("RouterProfile", &pname, "metadata.name", "missing name");
            continue;
        };
        if profiles.contains_key(&name) {
            report.error(
                "RouterProfile",
                &pname,
                "metadata.name",
                "profile names must be unique within one snapshot",
            );
            continue;
        }
        let s = &p.spec;
        if s.classifier.timeout_ms == 0 {
            report.error(
                "RouterProfile",
                &pname,
                "spec.classifier.timeoutMs",
                "must be > 0",
            );
        }
        if s.cost_model.default_output_tokens == 0 {
            report.error(
                "RouterProfile",
                &pname,
                "spec.costModel.defaultOutputTokens",
                "must be > 0",
            );
        }
        if s.classifier.type_ == routed_api::v1alpha1::ArtifactType::Onnx {
            // ADR-0016: an ONNX classifier needs both artifacts pinned; fail
            // at compile, not at profile load in the router.
            if s.classifier.uri.is_none() {
                report.error(
                    "RouterProfile",
                    &pname,
                    "spec.classifier.uri",
                    "type: onnx requires a model artifact uri",
                );
            }
            if s.classifier.tokenizer_uri.is_none() {
                report.error(
                    "RouterProfile",
                    &pname,
                    "spec.classifier.tokenizerUri",
                    "type: onnx requires a tokenizer artifact uri",
                );
            }
        }
        for (k, v) in &s
            .learned_router
            .as_ref()
            .map(|l| l.calibration.clone())
            .unwrap_or_default()
        {
            if k.parse::<f64>().is_err() || !within(*v, 0.0, 1.0) {
                report.error(
                    "RouterProfile",
                    &pname,
                    &format!("spec.learnedRouter.calibration.{k}"),
                    "keys must be numeric quality floors and values probabilities in 0..=1",
                );
            }
        }
        profiles.insert(
            name.clone(),
            CompiledProfile {
                name,
                classifier_type: s.classifier.type_,
                classifier_uri: s.classifier.uri.clone(),
                classifier_tokenizer_uri: s.classifier.tokenizer_uri.clone(),
                classifier_timeout_ms: s.classifier.timeout_ms,
                task_labels: s.classifier.labels.task.clone(),
                sensitivity_labels: s.classifier.labels.sensitivity.clone(),
                embedder_uri: s.embedder.as_ref().and_then(|e| e.uri.clone()),
                embedder_dimensions: s.embedder.as_ref().map(|e| e.dimensions),
                learned_router_uri: s.learned_router.as_ref().and_then(|l| l.uri.clone()),
                calibration: s
                    .learned_router
                    .as_ref()
                    .map(|l| l.calibration.clone())
                    .unwrap_or_default(),
                pii_detector_uri: s.security.pii_detector.as_ref().and_then(|a| a.uri.clone()),
                injection_detector_uri: s
                    .security
                    .injection_detector
                    .as_ref()
                    .and_then(|a| a.uri.clone()),
                default_output_tokens: s.cost_model.default_output_tokens,
            },
        );
    }

    // ---- data classes ----
    let mut data_classes = BTreeMap::new();
    let mut ranks: BTreeMap<u32, String> = BTreeMap::new();
    let mut header_owner: BTreeMap<String, String> = BTreeMap::new();
    for dc in &input.data_classes {
        let dname = ns_name(
            dc.metadata.namespace.as_deref(),
            dc.metadata.name.as_deref(),
        );
        let Some(name) = dc.metadata.name.clone() else {
            report.error("DataClass", &dname, "metadata.name", "missing name");
            continue;
        };
        if data_classes.contains_key(&name) {
            report.error(
                "DataClass",
                &dname,
                "metadata.name",
                "data class names must be unique within one snapshot",
            );
            continue;
        }
        let s: &DataClassSpec = &dc.spec;
        if !within(s.detection.min_confidence, 0.0, 1.0) {
            report.error(
                "DataClass",
                &dname,
                "spec.detection.minConfidence",
                "must be in 0..=1",
            );
        }
        if let Some(other) = ranks.insert(s.rank, name.clone()) {
            report.warn(
                "DataClass",
                &dname,
                "spec.rank",
                format!(
                    "rank {} is also used by {other}; most-restrictive-wins becomes ambiguous",
                    s.rank
                ),
            );
        }
        for hv in s
            .detection
            .header_values
            .iter()
            .map(|v| v.to_ascii_lowercase())
            .chain(std::iter::once(name.to_ascii_lowercase()))
        {
            if let Some(other) = header_owner.insert(hv.clone(), name.clone()) {
                if other != name {
                    report.error("DataClass", &dname, "spec.detection.headerValues", format!("header value {hv:?} is also claimed by data class {other:?}; X-Routed-Data-Class must resolve unambiguously"));
                }
            }
        }
        data_classes.insert(
            name.clone(),
            CompiledDataClass {
                name,
                rank: s.rank,
                header_values: s
                    .detection
                    .header_values
                    .iter()
                    .map(|v| v.to_ascii_lowercase())
                    .collect(),
                pii_entities: s.detection.pii_entities.iter().copied().collect(),
                min_confidence: s.detection.min_confidence,
                require_jurisdiction: s.constraints.require_jurisdiction.iter().copied().collect(),
                forbid_cloud_act_exposed: s.constraints.forbid_cloud_act_exposed,
                require_operator_control: s
                    .constraints
                    .require_operator_control
                    .iter()
                    .copied()
                    .collect(),
                forbid_capabilities: s.constraints.forbid_capabilities.iter().copied().collect(),
                max_retention_days: s.constraints.max_retention_days,
            },
        );
    }

    // ---- tiers ----
    let mut tiers = BTreeMap::new();
    let mut tier_ns: BTreeMap<String, String> = BTreeMap::new();
    for t in &input.tiers {
        let tname = ns_name(t.metadata.namespace.as_deref(), t.metadata.name.as_deref());
        let Some(name) = t.metadata.name.clone() else {
            report.error("ModelTier", &tname, "metadata.name", "missing name");
            continue;
        };
        if tiers.contains_key(&name) {
            report.error(
                "ModelTier",
                &tname,
                "metadata.name",
                "tier names must be unique within one snapshot",
            );
            continue;
        }
        let s: &ModelTierSpec = &t.spec;
        let mut ok = true;
        if s.gateway_model.trim().is_empty() {
            report.error(
                "ModelTier",
                &tname,
                "spec.gatewayModel",
                "must not be empty",
            );
            ok = false;
        }
        if s.context_window == 0 {
            report.error("ModelTier", &tname, "spec.contextWindow", "must be > 0");
            ok = false;
        }
        for (field, v) in [
            ("inputPerMillion", s.cost.input_per_million),
            ("outputPerMillion", s.cost.output_per_million),
        ] {
            if !within(v, 0.0, f64::MAX) {
                report.error(
                    "ModelTier",
                    &tname,
                    &format!("spec.cost.{field}"),
                    "must be a non-negative number",
                );
                ok = false;
            }
        }
        if !within(s.quality.baseline, 0.0, 1.0) {
            report.error(
                "ModelTier",
                &tname,
                "spec.quality.baseline",
                "must be in 0..=1",
            );
            ok = false;
        }
        for (task, q) in &s.quality.by_task {
            if !within(*q, 0.0, 1.0) {
                report.error(
                    "ModelTier",
                    &tname,
                    &format!("spec.quality.byTask.{task}"),
                    "must be in 0..=1",
                );
                ok = false;
            }
        }
        if !within(s.security.max_risk_score, 0.0, 1.0) {
            report.error(
                "ModelTier",
                &tname,
                "spec.security.maxRiskScore",
                "must be in 0..=1",
            );
            ok = false;
        }
        for dcn in &s.sovereignty.allowed_data_classes {
            if !data_classes.contains_key(dcn) {
                report.error(
                    "ModelTier",
                    &tname,
                    "spec.sovereignty.allowedDataClasses",
                    format!("data class {dcn:?} not found"),
                );
                ok = false;
            }
        }
        let (input_micro, output_micro) = if let (Some(i), Some(o)) = (
            to_micro_eur(s.cost.input_per_million, s.cost.currency, &fx),
            to_micro_eur(s.cost.output_per_million, s.cost.currency, &fx),
        ) {
            (i, o)
        } else {
            report.error(
                "ModelTier",
                &tname,
                "spec.cost.currency",
                format!(
                    "no conversion rate to EUR for {:?}; set RouterProfile.spec.costModel.fxToEUR",
                    s.cost.currency
                ),
            );
            ok = false;
            (0, 0)
        };
        if !ok {
            continue;
        }
        tier_ns.insert(
            name.clone(),
            t.metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "default".into()),
        );
        let compiled = CompiledTier {
            name: name.clone(),
            gateway_model: s.gateway_model.clone(),
            provider: s.provider.clone(),
            capabilities: s.capabilities.iter().copied().collect(),
            context_window: s.context_window,
            input_micro_eur_per_million: input_micro,
            output_micro_eur_per_million: output_micro,
            currency: s.cost.currency,
            quality_baseline: s.quality.baseline,
            quality_by_task: s.quality.by_task.clone(),
            latency_p50_ms: s.latency.p50_ms,
            latency_p95_ms: s.latency.p95_ms,
            jurisdiction: s.sovereignty.jurisdiction,
            data_residency: s.sovereignty.data_residency.clone(),
            operator_control: s.sovereignty.operator_control,
            cloud_act_exposed: s.sovereignty.cloud_act_exposed,
            allowed_data_classes: s.sovereignty.allowed_data_classes.iter().cloned().collect(),
            max_risk_score: s.security.max_risk_score,
            tool_calling_allowed: s.security.tool_calling_allowed,
            labels: s.labels.clone(),
        };
        // A tier that claims a data class must actually satisfy its constraints.
        for dcn in &compiled.allowed_data_classes {
            if let Some(dc) = data_classes.get(dcn) {
                if let Some(reason) = data_class_violation(&compiled, dc) {
                    report.warn("ModelTier", &tname, "spec.sovereignty.allowedDataClasses", format!("claims data class {dcn:?} but violates its constraints ({reason}); the engine will eliminate it for that class"));
                }
            }
        }
        tiers.insert(name, compiled);
    }

    // ---- policies ----
    let mut policies = Vec::new();
    let mut seen_keys = BTreeSet::new();
    for p in &input.policies {
        let pname = ns_name(p.metadata.namespace.as_deref(), p.metadata.name.as_deref());
        let (Some(name), namespace) = (
            p.metadata.name.clone(),
            p.metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "default".into()),
        ) else {
            report.error("RoutingPolicy", &pname, "metadata.name", "missing name");
            continue;
        };
        let key = format!("{namespace}/{name}");
        if !seen_keys.insert(key.clone()) {
            report.error("RoutingPolicy", &pname, "metadata.name", "duplicate policy");
            continue;
        }
        let s: &RoutingPolicySpec = &p.spec;
        let mut ok = true;

        // Candidate resolution: same namespace only.
        let in_ns = |tn: &str| tier_ns.get(tn).is_some_and(|ns| *ns == namespace);
        let mut candidates: BTreeSet<String> = tiers
            .values()
            .filter(|t| in_ns(&t.name))
            .filter(|t| {
                s.candidates
                    .tier_selector
                    .match_labels
                    .iter()
                    .all(|(k, v)| t.labels.get(k) == Some(v))
            })
            .map(|t| t.name.clone())
            .collect();
        if !s.candidates.tier_selector.match_labels.is_empty()
            && s.candidates.include.is_empty()
            && candidates.is_empty()
        {
            report.warn(
                "RoutingPolicy",
                &pname,
                "spec.candidates.tierSelector",
                "selector matches no tier",
            );
        }
        if !s.candidates.include.is_empty() && s.candidates.tier_selector.match_labels.is_empty() {
            candidates.clear();
        }
        for inc in &s.candidates.include {
            if in_ns(inc) {
                candidates.insert(inc.clone());
            } else {
                report.error(
                    "RoutingPolicy",
                    &pname,
                    "spec.candidates.include",
                    format!("tier {inc:?} not found in namespace {namespace}"),
                );
                ok = false;
            }
        }
        for exc in &s.candidates.exclude {
            if !in_ns(exc) {
                report.warn(
                    "RoutingPolicy",
                    &pname,
                    "spec.candidates.exclude",
                    format!("tier {exc:?} not found"),
                );
            }
            candidates.remove(exc);
        }
        if candidates.is_empty() {
            report.error(
                "RoutingPolicy",
                &pname,
                "spec.candidates",
                "no candidate tiers resolve",
            );
            ok = false;
        }

        if let Some(v) = s.hard_constraints.max_cost_per_request_eur {
            if !within(v, 0.0, f64::MAX) {
                report.error(
                    "RoutingPolicy",
                    &pname,
                    "spec.hardConstraints.maxCostPerRequestEUR",
                    "must be a non-negative number",
                );
                ok = false;
            }
        }
        if let Some(v) = s.hard_constraints.deny_if_risk_score_above {
            if !within(v, 0.0, 1.0) {
                report.error(
                    "RoutingPolicy",
                    &pname,
                    "spec.hardConstraints.denyIfRiskScoreAbove",
                    "must be in 0..=1",
                );
                ok = false;
            }
        }
        if let Some(v) = s.objective.quality_floor {
            if !within(v, 0.0, 1.0) {
                report.error(
                    "RoutingPolicy",
                    &pname,
                    "spec.objective.qualityFloor",
                    "must be in 0..=1",
                );
                ok = false;
            }
        }
        let weights = match &s.objective.weights {
            Some(w) => {
                if [w.cost, w.quality, w.latency]
                    .iter()
                    .any(|v| !within(*v, 0.0, f64::MAX))
                {
                    report.error(
                        "RoutingPolicy",
                        &pname,
                        "spec.objective.weights",
                        "weights must be non-negative numbers",
                    );
                    ok = false;
                }
                Weights {
                    cost: w.cost,
                    quality: w.quality,
                    latency: w.latency,
                }
                .normalised(s.objective.mode)
            }
            None => Weights::for_mode(s.objective.mode),
        };
        if s.learned_router.enabled {
            match &s.learned_router.profile {
                Some(pr) if profiles.contains_key(pr) => {}
                Some(pr) => {
                    report.error(
                        "RoutingPolicy",
                        &pname,
                        "spec.learnedRouter.profile",
                        format!("RouterProfile {pr:?} not found"),
                    );
                    ok = false;
                }
                None => {
                    report.error(
                        "RoutingPolicy",
                        &pname,
                        "spec.learnedRouter.profile",
                        "required when learnedRouter.enabled",
                    );
                    ok = false;
                }
            }
            if !within(s.learned_router.min_confidence, 0.0, 1.0) {
                report.error(
                    "RoutingPolicy",
                    &pname,
                    "spec.learnedRouter.minConfidence",
                    "must be in 0..=1",
                );
                ok = false;
            }
        }
        if let Some(ft) = &s.fallback_decision.tier {
            match tiers.get(ft).filter(|_| in_ns(ft)) {
                None => {
                    report.error(
                        "RoutingPolicy",
                        &pname,
                        "spec.fallbackDecision.tier",
                        format!("tier {ft:?} not found in namespace {namespace}"),
                    );
                    ok = false;
                }
                Some(tier) if s.hard_constraints.respect_data_class => {
                    for dc in data_classes.values() {
                        if let Some(reason) = data_class_violation(tier, dc) {
                            report.warn("RoutingPolicy", &pname, "spec.fallbackDecision.tier", format!("fallback tier {ft:?} cannot serve DataClass {:?} ({reason}); degraded requests in that class will be blocked", dc.name));
                        }
                    }
                }
                Some(_) => {}
            }
        }
        if s.match_.model_aliases.is_empty() {
            report.warn(
                "RoutingPolicy",
                &pname,
                "spec.match.modelAliases",
                "empty: no request will be routed by this policy (everything passes through)",
            );
        }
        if !ok {
            continue;
        }
        policies.push(CompiledPolicy {
            key,
            namespace,
            name,
            priority: s.priority,
            match_: CompiledMatch {
                tenants: s.match_.tenants.clone(),
                agents: s.match_.agents.clone(),
                paths: s.match_.paths.clone(),
                model_aliases: s.match_.model_aliases.clone(),
            },
            candidates: candidates.into_iter().collect(),
            respect_data_class: s.hard_constraints.respect_data_class,
            max_cost_micro_eur: s
                .hard_constraints
                .max_cost_per_request_eur
                .and_then(|v| to_micro_eur(v, Currency::Eur, &fx)),
            require_capabilities: s
                .hard_constraints
                .require_capabilities
                .iter()
                .copied()
                .collect(),
            deny_if_risk_score_above: s.hard_constraints.deny_if_risk_score_above,
            mode: s.objective.mode,
            quality_floor: s.objective.quality_floor,
            weights,
            learned_router: CompiledLearnedRouter {
                enabled: s.learned_router.enabled,
                profile: s.learned_router.profile.clone(),
                min_confidence: s.learned_router.min_confidence,
            },
            reasoning_enabled: s.reasoning_budget.enabled,
            reasoning_map: s.reasoning_budget.map.clone(),
            fallback_tier: s.fallback_decision.tier.clone(),
            explain: s.explain,
            overridable: s.overridable,
        });
    }

    // Evaluation order: priority desc, then namespace/name. Warn on shadowing.
    policies.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.key.cmp(&b.key)));
    {
        let mut groups: BTreeMap<(i32, String, String), Vec<String>> = BTreeMap::new();
        for p in &policies {
            let key = (
                p.priority,
                p.namespace.clone(),
                serde_json::to_string(&p.match_).unwrap_or_default(),
            );
            groups.entry(key).or_default().push(p.key.clone());
        }
        for keys in groups.values().filter(|k| k.len() > 1) {
            for shadowed in &keys[1..] {
                report.warn(
                    "RoutingPolicy",
                    shadowed,
                    "spec.priority",
                    format!(
                        "same priority and identical match as {}; {} always wins",
                        keys[0], keys[0]
                    ),
                );
            }
        }
    }

    if report.has_errors() {
        return Err(CompileError(report));
    }
    let core = SnapshotCore {
        schema_version: SCHEMA_VERSION,
        compiler_version: COMPILER_VERSION.to_string(),
        tiers,
        data_classes,
        policies,
        profiles,
    };
    Ok((Snapshot::from_core(core), report))
}

/// Why a tier cannot serve a data class, if it cannot. Shared with the engine's
/// semantics (the engine re-implements the same checks as elimination reasons).
#[must_use]
pub fn data_class_violation(tier: &CompiledTier, dc: &CompiledDataClass) -> Option<&'static str> {
    if !tier.allowed_data_classes.contains(&dc.name) {
        return Some("not in tier.sovereignty.allowedDataClasses");
    }
    if !dc.require_jurisdiction.is_empty() && !dc.require_jurisdiction.contains(&tier.jurisdiction)
    {
        return Some("jurisdiction not allowed");
    }
    if dc.forbid_cloud_act_exposed && tier.cloud_act_exposed {
        return Some("CLOUD Act exposed");
    }
    if !dc.require_operator_control.is_empty()
        && !dc.require_operator_control.contains(&tier.operator_control)
    {
        return Some("operator control not allowed");
    }
    if tier
        .capabilities
        .iter()
        .any(|c| dc.forbid_capabilities.contains(c))
    {
        return Some("offers a forbidden capability");
    }
    None
}

#[cfg(test)]
mod tests;
