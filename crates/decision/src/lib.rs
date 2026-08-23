// SPDX-License-Identifier: Apache-2.0
//! Decision engine: candidate pruning by hard constraints, scoring, selection,
//! and explanation.
//!
//! PURE. This crate must never depend on tokio, kube, ort, axum, hyper, tonic,
//! or reqwest. Determinism is a tested property: the same snapshot, input and
//! findings always produce the same [`Decision`].
//!
//! Pipeline (ADR-0003, fixed order):
//! 1. select the `RoutingPolicy` (scope match + priority; hint override only
//!    within the matching set),
//! 2. `PASS_THROUGH` unless the requested model is a routed alias,
//! 3. resolve the data class as the most restrictive of explicit and inferred,
//! 4. hard constraints in order, each recording eliminations,
//! 5. fallback decision when nothing survives (only if it satisfies the data class),
//! 6. score survivors, select, compute parameters, explain.

mod types;

pub use types::*;

use std::collections::BTreeMap;

use routed_snapshot::{
    Capability, CompiledDataClass, CompiledPolicy, CompiledTier, MicroEur, Snapshot,
};

/// Quality prediction hook for the learned router (ADR-0004 seam).
pub trait QualityPredictor {
    /// Predicted quality in `0..=1` and confidence in `0..=1` for a tier, if available.
    fn predict(&self, tier: &CompiledTier, findings: &Findings) -> Option<(f64, f64)>;
}

/// Predictor that never predicts; the engine falls back to tier priors.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPredictor;

/// Arc-shareable predictor handle, so services can hold one concrete engine
/// type (`Engine<SharedPredictor>`) regardless of the implementation.
pub type SharedPredictor = std::sync::Arc<dyn QualityPredictor + Send + Sync>;

impl QualityPredictor for SharedPredictor {
    fn predict(&self, tier: &CompiledTier, findings: &Findings) -> Option<(f64, f64)> {
        (**self).predict(tier, findings)
    }
}

impl QualityPredictor for NoPredictor {
    fn predict(&self, _tier: &CompiledTier, _findings: &Findings) -> Option<(f64, f64)> {
        None
    }
}

/// The engine. Stateless apart from the optional predictor.
pub struct Engine<P: QualityPredictor = NoPredictor> {
    predictor: P,
}

impl Default for Engine<NoPredictor> {
    fn default() -> Self {
        Self {
            predictor: NoPredictor,
        }
    }
}

impl Engine<NoPredictor> {
    /// Engine using tier quality priors only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<P: QualityPredictor> Engine<P> {
    /// Engine with a learned quality predictor.
    pub fn with_predictor(predictor: P) -> Self {
        Self { predictor }
    }

    /// Make a decision. Never panics; never performs I/O.
    #[must_use]
    pub fn decide(
        &self,
        snapshot: &Snapshot,
        input: &DecisionInput,
        findings: &Findings,
        ctx: &DecisionContext,
    ) -> Decision {
        let mut d = Decision::new(ctx, input, findings, &snapshot.hash);

        // 1. policy selection
        let matching: Vec<&CompiledPolicy> = snapshot
            .core
            .policies
            .iter()
            .filter(|p| scope_matches(p, input))
            .collect();
        let Some(policy) = select_policy(&matching, input.hints.policy.as_deref(), &mut d) else {
            d.outcome = Outcome::PassThrough;
            d.reason = Some("no RoutingPolicy matches the request scope".into());
            return d;
        };
        d.policy = Some(policy.key.clone());

        // 2. routed alias?
        if !policy
            .match_
            .model_aliases
            .iter()
            .any(|a| glob_match(a, &input.requested_model))
        {
            d.outcome = Outcome::PassThrough;
            d.reason = Some(format!(
                "requested model {:?} is not a routed alias of {}",
                input.requested_model, policy.key
            ));
            return d;
        }

        // 3. data class
        let data_class = resolve_data_class(snapshot, input, findings, &mut d);
        d.data_class = data_class.map(|dc| dc.name.clone());

        let output_tokens = input.estimated_output_tokens;
        let input_tokens = input.estimated_input_tokens;

        // 4a. risk block: evaluated whenever a score exists, even if other classifiers degraded.
        if let (Some(threshold), Some(risk)) =
            (policy.deny_if_risk_score_above, findings.risk_score)
        {
            if risk > threshold {
                for name in &policy.candidates {
                    d.candidates.push(Candidate::eliminated(
                        name,
                        EliminationReason::DenyIfRiskScoreAbove,
                    ));
                }
                d.outcome = Outcome::Block;
                d.reason = Some(format!(
                    "risk score {} exceeds denyIfRiskScoreAbove {}",
                    round6(risk),
                    threshold
                ));
                return d;
            }
        }

        // 4b. degraded classification => fallback path. A missing risk score counts as
        // degraded whenever the policy or any candidate depends on it (never permissive).
        let risk_needed = policy.deny_if_risk_score_above.is_some()
            || policy
                .candidates
                .iter()
                .filter_map(|n| snapshot.tier(n))
                .any(|t| t.max_risk_score < 1.0);
        if findings.risk_score.is_none()
            && risk_needed
            && !d.degraded.iter().any(|x| x == "risk:missing")
        {
            d.degraded.push("risk:missing".into());
        }
        if !d.degraded.is_empty() {
            d.fallback = true;
            return self.fallback_or_block(
                snapshot,
                policy,
                input,
                findings,
                data_class,
                &mut d,
                "classification degraded",
                true,
            );
        }

        // 4c-e. hard constraints
        let mut survivors: Vec<&CompiledTier> = Vec::new();
        for name in &policy.candidates {
            let Some(tier) = snapshot.tier(name) else {
                continue;
            };
            match hard_constraint_violation(
                tier,
                policy,
                input,
                findings,
                data_class,
                input_tokens,
                output_tokens,
            ) {
                Some(reason) => d.candidates.push(Candidate::eliminated(name, reason)),
                None => survivors.push(tier),
            }
        }

        // 5. nothing survives => fallback (still subject to every hard constraint)
        if survivors.is_empty() {
            d.fallback = true;
            return self.fallback_or_block(
                snapshot,
                policy,
                input,
                findings,
                data_class,
                &mut d,
                "no candidate satisfies the hard constraints",
                false,
            );
        }

        // 6. score
        self.score_and_select(
            policy,
            &survivors,
            findings,
            input_tokens,
            output_tokens,
            &mut d,
        );
        d
    }

    /// Route to the policy fallback tier or block.
    ///
    /// When classification is degraded the cost cap and quality floor are
    /// relaxed (they depend on estimates the engine can still compute, but the
    /// fallback is by definition the "safe" tier); the data class, request facts
    /// (tools, context window, required capabilities) and any known risk score
    /// are always enforced. When classification succeeded but nothing survived,
    /// every hard constraint applies to the fallback too.
    #[allow(clippy::too_many_arguments)]
    fn fallback_or_block(
        &self,
        snapshot: &Snapshot,
        policy: &CompiledPolicy,
        input: &DecisionInput,
        findings: &Findings,
        data_class: Option<&CompiledDataClass>,
        d: &mut Decision,
        why: &str,
        degraded: bool,
    ) -> Decision {
        let Some(fb_name) = &policy.fallback_tier else {
            d.outcome = Outcome::Block;
            d.reason = Some(format!("{why} and the policy has no fallbackDecision.tier"));
            return d.clone();
        };
        let Some(tier) = snapshot.tier(fb_name) else {
            d.outcome = Outcome::Block;
            d.reason = Some(format!(
                "{why} and fallback tier {fb_name:?} is not in the snapshot"
            ));
            return d.clone();
        };
        let violation = if degraded {
            request_fact_violation(tier, policy, input, findings, data_class)
        } else {
            hard_constraint_violation(
                tier,
                policy,
                input,
                findings,
                data_class,
                input.estimated_input_tokens,
                input.estimated_output_tokens,
            )
        };
        if let Some(reason) = violation {
            if !d.candidates.iter().any(|c| c.tier == tier.name) {
                d.candidates.push(Candidate::eliminated(&tier.name, reason));
            }
            d.candidates.sort_by(|a, b| a.tier.cmp(&b.tier));
            d.outcome = Outcome::Block;
            d.reason = Some(format!(
                "{why}; fallback tier {fb_name:?} eliminated by {reason}"
            ));
            return d.clone();
        }
        let cost = tier.estimate_cost(input.estimated_input_tokens, input.estimated_output_tokens);
        let quality = self.predicted_quality(tier, findings, policy);
        if let Some(prev) = d
            .candidates
            .iter()
            .find(|c| c.tier == tier.name)
            .and_then(|c| c.eliminated_by)
        {
            d.notes.push(format!("fallback tier {fb_name:?} was eliminated by {prev}; relaxed because classification is degraded"));
        }
        d.candidates.retain(|c| c.tier != tier.name);
        d.candidates.push(Candidate {
            tier: tier.name.clone(),
            eliminated_by: None,
            predicted_quality: Some(round6(quality)),
            estimated_cost_eur: Some(micro_to_eur(cost)),
            score: None,
            selected: true,
        });
        d.candidates.sort_by(|a, b| a.tier.cmp(&b.tier));
        d.outcome = Outcome::Route;
        d.reason = Some(format!("{why}; using fallbackDecision.tier"));
        d.selected_tier = Some(tier.name.clone());
        d.gateway_model = Some(tier.gateway_model.clone());
        d.estimated_cost_eur = Some(micro_to_eur(cost));
        d.estimated_savings_eur = Some(0.0);
        d.parameters = parameters_for(policy, findings);
        d.clone()
    }

    fn predicted_quality(
        &self,
        tier: &CompiledTier,
        findings: &Findings,
        policy: &CompiledPolicy,
    ) -> f64 {
        if policy.learned_router.enabled {
            if let Some((q, conf)) = self.predictor.predict(tier, findings) {
                if conf >= policy.learned_router.min_confidence && q.is_finite() {
                    return q.clamp(0.0, 1.0);
                }
            }
        }
        tier.quality_for(findings.task.as_deref())
    }

    fn score_and_select(
        &self,
        policy: &CompiledPolicy,
        survivors: &[&CompiledTier],
        findings: &Findings,
        input_tokens: u64,
        output_tokens: u64,
        d: &mut Decision,
    ) {
        struct Scored<'a> {
            tier: &'a CompiledTier,
            quality: f64,
            cost: MicroEur,
            latency: u64,
        }
        let mut scored: Vec<Scored<'_>> = survivors
            .iter()
            .map(|t| Scored {
                tier: t,
                quality: self.predicted_quality(t, findings, policy),
                cost: t.estimate_cost(input_tokens, output_tokens),
                latency: t.latency_p50_ms,
            })
            .collect();

        // quality floor: drop below floor unless none remain, then keep the best quality
        if let Some(floor) = policy.quality_floor {
            let above: Vec<bool> = scored.iter().map(|s| s.quality + 1e-9 >= floor).collect();
            if above.iter().any(|a| *a) {
                let mut kept = Vec::new();
                for (s, ok) in scored.into_iter().zip(above) {
                    if ok {
                        kept.push(s);
                    } else {
                        d.candidates.push(Candidate {
                            tier: s.tier.name.clone(),
                            eliminated_by: Some(EliminationReason::QualityFloor),
                            predicted_quality: Some(round6(s.quality)),
                            estimated_cost_eur: Some(micro_to_eur(s.cost)),
                            score: None,
                            selected: false,
                        });
                    }
                }
                scored = kept;
            } else {
                let best = scored.iter().map(|s| s.quality).fold(f64::MIN, f64::max);
                let mut kept = Vec::new();
                for s in scored {
                    if (s.quality - best).abs() < 1e-12 {
                        kept.push(s);
                    } else {
                        d.candidates.push(Candidate {
                            tier: s.tier.name.clone(),
                            eliminated_by: Some(EliminationReason::QualityFloor),
                            predicted_quality: Some(round6(s.quality)),
                            estimated_cost_eur: Some(micro_to_eur(s.cost)),
                            score: None,
                            selected: false,
                        });
                    }
                }
                scored = kept;
                d.reason = Some(format!(
                    "no candidate meets qualityFloor {floor}; keeping the best available quality"
                ));
            }
        }

        let (min_c, max_c) = minmax(scored.iter().map(|s| s.cost as f64));
        let (min_q, max_q) = minmax(scored.iter().map(|s| s.quality));
        let (min_l, max_l) = minmax(scored.iter().map(|s| s.latency as f64));
        let w = policy.weights;
        let mut results: Vec<(f64, &Scored<'_>)> = scored
            .iter()
            .map(|s| {
                let cost_n = norm(s.cost as f64, min_c, max_c);
                let q_n = norm(s.quality, min_q, max_q);
                let lat_n = norm(s.latency as f64, min_l, max_l);
                let score = w.cost * (1.0 - cost_n) + w.quality * q_n + w.latency * (1.0 - lat_n);
                (round6(score), s)
            })
            .collect();
        // deterministic: score desc, cost asc, name asc
        results.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cost.cmp(&b.1.cost))
                .then(a.1.tier.name.cmp(&b.1.tier.name))
        });
        let Some((_, best)) = results.first() else {
            return;
        };
        let best_name = best.tier.name.clone();
        let max_cost = results
            .iter()
            .map(|(_, s)| s.cost)
            .max()
            .unwrap_or(best.cost);

        for (score, s) in &results {
            d.candidates.push(Candidate {
                tier: s.tier.name.clone(),
                eliminated_by: None,
                predicted_quality: Some(round6(s.quality)),
                estimated_cost_eur: Some(micro_to_eur(s.cost)),
                score: Some(*score),
                selected: s.tier.name == best_name,
            });
        }
        d.candidates.sort_by(|a, b| a.tier.cmp(&b.tier));
        d.outcome = Outcome::Route;
        d.selected_tier = Some(best_name);
        d.gateway_model = Some(best.tier.gateway_model.clone());
        d.estimated_cost_eur = Some(micro_to_eur(best.cost));
        d.estimated_savings_eur = Some(micro_to_eur(max_cost.saturating_sub(best.cost)));
        d.parameters = parameters_for(policy, findings);
    }
}

fn minmax(it: impl Iterator<Item = f64>) -> (f64, f64) {
    it.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
        (lo.min(v), hi.max(v))
    })
}

fn norm(v: f64, lo: f64, hi: f64) -> f64 {
    if hi <= lo || !hi.is_finite() || !lo.is_finite() || !v.is_finite() {
        0.0
    } else {
        ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

/// Round to six decimals (explanation stability across architectures).
#[must_use]
pub fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// Micro-EUR to EUR with eight decimals.
#[must_use]
pub fn micro_to_eur(m: MicroEur) -> f64 {
    (m as f64) / 1e6
}

/// Glob match: `*` matches everything, `prefix*` and `*suffix` are supported, otherwise exact.
#[must_use]
pub fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    pattern == value
}

fn list_matches(patterns: &[String], value: Option<&str>) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let v = value.unwrap_or("");
    patterns.iter().any(|p| glob_match(p, v))
}

/// Whether a policy's scope (tenant, agent, path) matches the request.
#[must_use]
pub fn scope_matches(policy: &CompiledPolicy, input: &DecisionInput) -> bool {
    list_matches(&policy.match_.tenants, input.tenant.as_deref())
        && list_matches(&policy.match_.agents, input.agent.as_deref())
        && list_matches(&policy.match_.paths, Some(&input.path))
}

fn select_policy<'a>(
    matching: &[&'a CompiledPolicy],
    override_: Option<&str>,
    d: &mut Decision,
) -> Option<&'a CompiledPolicy> {
    let winner = matching.first().copied();
    if let Some(name) = override_ {
        match matching.iter().find(|p| p.key == name || p.name == name) {
            Some(p) if p.overridable || Some(p.key.as_str()) == winner.map(|w| w.key.as_str()) => {
                return Some(p);
            }
            Some(p) => d.notes.push(format!(
                "ignored X-Routed-Policy {name:?}: {} is not overridable",
                p.key
            )),
            None => d.notes.push(format!(
                "ignored X-Routed-Policy {name:?}: not in the set of policies matching this request"
            )),
        }
    }
    winner
}

fn resolve_data_class<'a>(
    snapshot: &'a Snapshot,
    input: &DecisionInput,
    findings: &Findings,
    d: &mut Decision,
) -> Option<&'a CompiledDataClass> {
    let classes = &snapshot.core.data_classes;
    let mut best: Option<&CompiledDataClass> = None;
    let mut consider = |dc: &'a CompiledDataClass| {
        if best.is_none_or(|b| dc.rank > b.rank) {
            best = Some(dc);
        }
    };
    // explicit header values (restriction only: merged by max rank; every matching class counts)
    for h in &input.hints.data_classes {
        let h = h.trim().to_ascii_lowercase();
        let mut matched = false;
        for dc in classes
            .values()
            .filter(|dc| dc.name.eq_ignore_ascii_case(&h) || dc.header_values.contains(&h))
        {
            matched = true;
            consider(dc);
        }
        if !matched {
            d.notes.push(format!(
                "ignored X-Routed-Data-Class {h:?}: unknown data class"
            ));
        }
    }
    // inferred sensitivity label
    if let Some(label) = &findings.inferred_data_class {
        if let Some(dc) = classes.get(label) {
            consider(dc);
        }
    }
    // inferred from PII entities, honouring the class confidence threshold
    for dc in classes.values() {
        let hit = findings.pii_entities.iter().any(|e| {
            dc.pii_entities.contains(e)
                && findings.pii_confidence.get(e).copied().unwrap_or(1.0) + 1e-9
                    >= dc.min_confidence
        });
        if hit {
            consider(dc);
        }
    }
    best
}

/// Constraints that depend only on request facts and the data class (applied to
/// the fallback tier even when classification is degraded).
#[must_use]
pub fn request_fact_violation(
    tier: &CompiledTier,
    policy: &CompiledPolicy,
    input: &DecisionInput,
    findings: &Findings,
    data_class: Option<&CompiledDataClass>,
) -> Option<EliminationReason> {
    if policy.respect_data_class {
        if let Some(dc) = data_class {
            if let Some(r) = data_class_violation(tier, dc) {
                return Some(r);
            }
        }
    }
    if !policy
        .require_capabilities
        .iter()
        .all(|c| tier.capabilities.contains(c))
    {
        return Some(EliminationReason::RequireCapabilities);
    }
    if input.tools_present && !tier.capabilities.contains(&Capability::Tools) {
        return Some(EliminationReason::ToolsNotSupported);
    }
    if tier.context_window
        < input
            .estimated_input_tokens
            .saturating_add(input.estimated_output_tokens)
    {
        return Some(EliminationReason::ContextWindow);
    }
    if let Some(risk) = findings.risk_score {
        if risk > tier.max_risk_score {
            return Some(EliminationReason::MaxRiskScore);
        }
    }
    if input.tools_present && !tier.tool_calling_allowed {
        return Some(EliminationReason::ToolCallingNotAllowed);
    }
    None
}

/// Why a tier cannot serve a data class, as an elimination reason.
#[must_use]
pub fn data_class_violation(
    tier: &CompiledTier,
    dc: &CompiledDataClass,
) -> Option<EliminationReason> {
    if !tier.allowed_data_classes.contains(&dc.name) {
        return Some(EliminationReason::DataClassNotAllowed);
    }
    if !dc.require_jurisdiction.is_empty() && !dc.require_jurisdiction.contains(&tier.jurisdiction)
    {
        return Some(EliminationReason::DataClassRequireJurisdiction);
    }
    if dc.forbid_cloud_act_exposed && tier.cloud_act_exposed {
        return Some(EliminationReason::DataClassForbidCloudActExposed);
    }
    if !dc.require_operator_control.is_empty()
        && !dc.require_operator_control.contains(&tier.operator_control)
    {
        return Some(EliminationReason::DataClassRequireOperatorControl);
    }
    if tier
        .capabilities
        .iter()
        .any(|c| dc.forbid_capabilities.contains(c))
    {
        return Some(EliminationReason::DataClassForbidCapabilities);
    }
    None
}

/// First hard constraint a tier violates, in the fixed evaluation order.
#[must_use]
pub fn hard_constraint_violation(
    tier: &CompiledTier,
    policy: &CompiledPolicy,
    input: &DecisionInput,
    findings: &Findings,
    data_class: Option<&CompiledDataClass>,
    input_tokens: u64,
    output_tokens: u64,
) -> Option<EliminationReason> {
    // 2. data class
    if policy.respect_data_class {
        if let Some(dc) = data_class {
            if let Some(r) = data_class_violation(tier, dc) {
                return Some(r);
            }
        }
    }
    // 3. capabilities / context window
    if !policy
        .require_capabilities
        .iter()
        .all(|c| tier.capabilities.contains(c))
    {
        return Some(EliminationReason::RequireCapabilities);
    }
    if input.tools_present && !tier.capabilities.contains(&Capability::Tools) {
        return Some(EliminationReason::ToolsNotSupported);
    }
    if tier.context_window < input_tokens.saturating_add(output_tokens) {
        return Some(EliminationReason::ContextWindow);
    }
    // 4. tier security
    if let Some(risk) = findings.risk_score {
        if risk > tier.max_risk_score {
            return Some(EliminationReason::MaxRiskScore);
        }
    }
    if input.tools_present && !tier.tool_calling_allowed {
        return Some(EliminationReason::ToolCallingNotAllowed);
    }
    // 5. cost cap
    if let Some(cap) = policy.max_cost_micro_eur {
        if tier.estimate_cost(input_tokens, output_tokens) > cap {
            return Some(EliminationReason::MaxCostPerRequest);
        }
    }
    None
}

fn parameters_for(policy: &CompiledPolicy, findings: &Findings) -> Parameters {
    let reasoning = if policy.reasoning_enabled {
        findings
            .complexity
            .and_then(|c| policy.reasoning_map.get(&c).copied())
    } else {
        None
    };
    Parameters {
        reasoning,
        max_tokens: None,
    }
}

/// Whether the request would be routed (a policy matches and the requested
/// model is one of its routed aliases). Lets ingress skip classification for
/// pass-through traffic.
#[must_use]
pub fn is_routed(snapshot: &Snapshot, input: &DecisionInput) -> bool {
    let matching: Vec<&CompiledPolicy> = snapshot
        .core
        .policies
        .iter()
        .filter(|p| scope_matches(p, input))
        .collect();
    let mut scratch = Decision::new(
        &DecisionContext { id: String::new() },
        input,
        &Findings::default(),
        &snapshot.hash,
    );
    select_policy(&matching, input.hints.policy.as_deref(), &mut scratch).is_some_and(|p| {
        p.match_
            .model_aliases
            .iter()
            .any(|a| glob_match(a, &input.requested_model))
    })
}

/// Build an index of candidate tiers by name (used by tests and tooling).
#[must_use]
pub fn candidates_by_name(d: &Decision) -> BTreeMap<&str, &Candidate> {
    d.candidates.iter().map(|c| (c.tier.as_str(), c)).collect()
}

#[cfg(test)]
mod tests;
