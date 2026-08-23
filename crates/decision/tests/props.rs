// SPDX-License-Identifier: Apache-2.0
//! Property tests: hard constraints are never violated by the selected tier
//! for any random policy / tier / data-class set; hints only restrict;
//! decisions are deterministic; explanations are complete.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use routed_decision::{
    Decision, DecisionContext, DecisionInput, Engine, Findings, Outcome, PiiEntity, RequestHints,
};
use routed_snapshot::{
    Capability, CompiledDataClass, CompiledLearnedRouter, CompiledMatch, CompiledPolicy,
    CompiledTier, Currency, Jurisdiction, ObjectiveMode, OperatorControl, SCHEMA_VERSION, Snapshot,
    SnapshotCore, Weights,
};

const CAPS: [Capability; 4] = [
    Capability::Chat,
    Capability::Tools,
    Capability::Json,
    Capability::Vision,
];
const JUR: [Jurisdiction; 3] = [Jurisdiction::Eu, Jurisdiction::Us, Jurisdiction::Other];
const OPS: [OperatorControl; 3] = [
    OperatorControl::EuEntity,
    OperatorControl::UsEntity,
    OperatorControl::OnPrem,
];
const MODES: [ObjectiveMode; 4] = [
    ObjectiveMode::CostFirstWithQualityFloor,
    ObjectiveMode::QualityFirst,
    ObjectiveMode::Balanced,
    ObjectiveMode::LatencyFirst,
];
const DC_NAMES: [&str; 3] = ["public", "internal", "personal"];

fn arb_tier(idx: usize) -> impl Strategy<Value = CompiledTier> {
    (
        proptest::collection::btree_set(proptest::sample::select(&CAPS[..]), 0..=4),
        1_000u64..200_000,
        0u64..10_000_000, // micro-EUR per million
        0u64..30_000_000,
        0.0f64..=1.0,
        0u64..3000,
        proptest::sample::select(&JUR[..]),
        proptest::sample::select(&OPS[..]),
        any::<bool>(),
        proptest::collection::btree_set(proptest::sample::select(&DC_NAMES[..]), 0..=3),
        0.0f64..=1.0,
        any::<bool>(),
    )
        .prop_map(
            move |(caps, cw, inp, out, q, lat, jur, op, cloud, allowed, max_risk, tools_ok)| {
                CompiledTier {
                    name: format!("t{idx}"),
                    gateway_model: format!("m{idx}"),
                    provider: None,
                    capabilities: caps,
                    context_window: cw,
                    input_micro_eur_per_million: inp,
                    output_micro_eur_per_million: out,
                    currency: Currency::Eur,
                    quality_baseline: q,
                    quality_by_task: BTreeMap::new(),
                    latency_p50_ms: lat,
                    latency_p95_ms: lat * 2,
                    jurisdiction: jur,
                    data_residency: None,
                    operator_control: op,
                    cloud_act_exposed: cloud,
                    allowed_data_classes: allowed.into_iter().map(String::from).collect(),
                    max_risk_score: max_risk,
                    tool_calling_allowed: tools_ok,
                    labels: BTreeMap::new(),
                }
            },
        )
}

fn arb_data_class(name: &'static str, rank: u32) -> impl Strategy<Value = CompiledDataClass> {
    (
        proptest::collection::btree_set(proptest::sample::select(&JUR[..]), 0..=2),
        any::<bool>(),
        proptest::collection::btree_set(proptest::sample::select(&OPS[..]), 0..=2),
        proptest::collection::btree_set(proptest::sample::select(&CAPS[..]), 0..=1),
        any::<bool>(),
    )
        .prop_map(move |(rj, cloud, rop, fcaps, pii)| CompiledDataClass {
            name: name.into(),
            rank,
            header_values: BTreeSet::from([name.to_string()]),
            pii_entities: if pii {
                BTreeSet::from([PiiEntity::Email])
            } else {
                BTreeSet::new()
            },
            min_confidence: 0.7,
            require_jurisdiction: rj,
            forbid_cloud_act_exposed: cloud,
            require_operator_control: rop,
            forbid_capabilities: fcaps,
            max_retention_days: None,
        })
}

#[derive(Debug, Clone)]
struct World {
    snapshot: Snapshot,
    input: DecisionInput,
    findings: Findings,
}

fn arb_world() -> impl Strategy<Value = World> {
    let tiers = (1usize..=6).prop_flat_map(|n| (0..n).map(arb_tier).collect::<Vec<_>>());
    let classes = (
        arb_data_class("public", 0),
        arb_data_class("internal", 1),
        arb_data_class("personal", 3),
    );
    let policy = (
        proptest::option::of(0u64..200_000), // cost cap micro-EUR
        proptest::collection::btree_set(proptest::sample::select(&CAPS[..]), 0..=2),
        proptest::option::of(0.0f64..=1.0),
        proptest::sample::select(&MODES[..]),
        proptest::option::of(0.0f64..=1.0),
        any::<bool>(),                   // respect data class
        proptest::option::of(0usize..6), // fallback tier index
        any::<bool>(),                   // overridable
    );
    let request = (
        any::<bool>(),
        1u64..50_000,
        1u64..2_000,
        proptest::option::of(proptest::sample::select(&DC_NAMES[..])),
        proptest::option::of(0.0f64..=1.0),
        any::<bool>(), // pii email present
        any::<bool>(), // degraded
        any::<bool>(), // policy hint present
    );
    (tiers, classes, policy, request).prop_map(
        |(
            tiers,
            (c0, c1, c2),
            (cap, rcaps, deny, mode, floor, respect, fb, overridable),
            (tools, inp, out, dc, risk, pii, degraded, hint_policy),
        )| {
            let fallback = fb.and_then(|i| tiers.get(i).map(|t| t.name.clone()));
            let policy = CompiledPolicy {
                key: "ns/p".into(),
                namespace: "ns".into(),
                name: "p".into(),
                priority: 1,
                match_: CompiledMatch {
                    tenants: vec![],
                    agents: vec![],
                    paths: vec![],
                    model_aliases: vec!["auto".into()],
                },
                candidates: tiers.iter().map(|t| t.name.clone()).collect(),
                respect_data_class: respect,
                max_cost_micro_eur: cap,
                require_capabilities: rcaps,
                deny_if_risk_score_above: deny,
                mode,
                quality_floor: floor,
                weights: Weights::for_mode(mode),
                learned_router: CompiledLearnedRouter::default(),
                reasoning_enabled: false,
                reasoning_map: BTreeMap::new(),
                fallback_tier: fallback,
                explain: true,
                overridable,
            };
            let core = SnapshotCore {
                schema_version: SCHEMA_VERSION,
                compiler_version: "prop".into(),
                tiers: tiers.into_iter().map(|t| (t.name.clone(), t)).collect(),
                data_classes: [c0, c1, c2]
                    .into_iter()
                    .map(|c| (c.name.clone(), c))
                    .collect(),
                policies: vec![policy],
                profiles: BTreeMap::new(),
            };
            World {
                snapshot: Snapshot::from_core(core),
                input: DecisionInput {
                    tenant: None,
                    agent: None,
                    path: "/v1/chat/completions".into(),
                    requested_model: "auto".into(),
                    tools_present: tools,
                    estimated_input_tokens: inp,
                    estimated_output_tokens: out,
                    hints: RequestHints {
                        data_classes: dc.into_iter().map(String::from).collect(),
                        policy: if hint_policy { Some("p".into()) } else { None },
                        dry_run: false,
                    },
                },
                findings: Findings {
                    task: None,
                    complexity: None,
                    risk_score: risk,
                    pii_entities: if pii {
                        BTreeSet::from([PiiEntity::Email])
                    } else {
                        BTreeSet::new()
                    },
                    pii_confidence: BTreeMap::new(),
                    inferred_data_class: None,
                    degraded: if degraded {
                        vec!["classifier".into()]
                    } else {
                        vec![]
                    },
                },
            }
        },
    )
}

/// Independent re-implementation of every hard constraint (not engine code).
fn independently_ok(
    w: &World,
    tier: &CompiledTier,
    dc: Option<&CompiledDataClass>,
    check_non_dc: bool,
) -> bool {
    let p = &w.snapshot.core.policies[0];
    if p.respect_data_class {
        if let Some(dc) = dc {
            if !tier.allowed_data_classes.contains(&dc.name) {
                return false;
            }
            if !dc.require_jurisdiction.is_empty()
                && !dc.require_jurisdiction.contains(&tier.jurisdiction)
            {
                return false;
            }
            if dc.forbid_cloud_act_exposed && tier.cloud_act_exposed {
                return false;
            }
            if !dc.require_operator_control.is_empty()
                && !dc.require_operator_control.contains(&tier.operator_control)
            {
                return false;
            }
            if tier
                .capabilities
                .iter()
                .any(|c| dc.forbid_capabilities.contains(c))
            {
                return false;
            }
        }
    }
    if !check_non_dc {
        return true;
    }
    if !p
        .require_capabilities
        .iter()
        .all(|c| tier.capabilities.contains(c))
    {
        return false;
    }
    if w.input.tools_present
        && (!tier.capabilities.contains(&Capability::Tools) || !tier.tool_calling_allowed)
    {
        return false;
    }
    if tier.context_window < w.input.estimated_input_tokens + w.input.estimated_output_tokens {
        return false;
    }
    if let Some(r) = w.findings.risk_score {
        if r > tier.max_risk_score {
            return false;
        }
    }
    if let Some(cap) = p.max_cost_micro_eur {
        let cost = (u128::from(w.input.estimated_input_tokens)
            * u128::from(tier.input_micro_eur_per_million)
            + u128::from(w.input.estimated_output_tokens)
                * u128::from(tier.output_micro_eur_per_million))
        .div_ceil(1_000_000);
        if cost > u128::from(cap) {
            return false;
        }
    }
    true
}

/// Request-fact constraints (tools, context window, required capabilities, known risk).
fn request_facts_ok(w: &World, tier: &CompiledTier) -> bool {
    let p = &w.snapshot.core.policies[0];
    if !p
        .require_capabilities
        .iter()
        .all(|c| tier.capabilities.contains(c))
    {
        return false;
    }
    if w.input.tools_present
        && (!tier.capabilities.contains(&Capability::Tools) || !tier.tool_calling_allowed)
    {
        return false;
    }
    if tier.context_window < w.input.estimated_input_tokens + w.input.estimated_output_tokens {
        return false;
    }
    if let Some(r) = w.findings.risk_score {
        if r > tier.max_risk_score {
            return false;
        }
    }
    true
}

/// Whether the engine must treat classification as degraded for this world.
fn effectively_degraded(w: &World) -> bool {
    let p = &w.snapshot.core.policies[0];
    let risk_needed = p.deny_if_risk_score_above.is_some()
        || w.snapshot
            .core
            .tiers
            .values()
            .any(|t| t.max_risk_score < 1.0);
    !w.findings.degraded.is_empty() || (w.findings.risk_score.is_none() && risk_needed)
}

/// Effective data class computed independently: max rank of header and PII inference.
fn effective_class<'a>(w: &'a World) -> Option<&'a CompiledDataClass> {
    let classes = &w.snapshot.core.data_classes;
    let mut best: Option<&CompiledDataClass> = None;
    let mut consider = |dc: &'a CompiledDataClass| {
        if best.is_none_or(|b| dc.rank > b.rank) {
            best = Some(dc);
        }
    };
    for h in &w.input.hints.data_classes {
        if let Some(dc) = classes.get(h) {
            consider(dc);
        }
    }
    for dc in classes.values() {
        if !dc.pii_entities.is_empty()
            && w.findings
                .pii_entities
                .iter()
                .any(|e| dc.pii_entities.contains(e))
        {
            consider(dc);
        }
    }
    best
}

fn run(w: &World) -> Decision {
    Engine::new().decide(
        &w.snapshot,
        &w.input,
        &w.findings,
        &DecisionContext { id: "p".into() },
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn selected_tier_never_violates_hard_constraints(w in arb_world()) {
        let d = run(&w);
        let dc = effective_class(&w);
        prop_assert_eq!(d.data_class.as_deref(), dc.map(|c| c.name.as_str()));
        if d.outcome == Outcome::Route {
            let name = d.selected_tier.clone().unwrap();
            let tier = w.snapshot.tier(&name).unwrap();
            if d.fallback {
                prop_assert_eq!(Some(&name), w.snapshot.core.policies[0].fallback_tier.as_ref());
                prop_assert!(independently_ok(&w, tier, dc, false), "fallback tier violates the data class");
                prop_assert!(request_facts_ok(&w, tier), "fallback tier violates request facts: {}", d.to_json());
                if !effectively_degraded(&w) {
                    prop_assert!(independently_ok(&w, tier, dc, true), "non-degraded fallback violates a hard constraint: {}", d.to_json());
                }
            } else {
                prop_assert!(independently_ok(&w, tier, dc, true), "selected tier violates a hard constraint: {}", d.to_json());
                if let (Some(t), Some(r)) = (w.snapshot.core.policies[0].deny_if_risk_score_above, w.findings.risk_score) {
                    prop_assert!(r <= t);
                }
            }
        }
    }

    #[test]
    fn all_fail_implies_never_route_without_fallback(w in arb_world()) {
        let d = run(&w);
        let dc = effective_class(&w);
        let any_ok = w.snapshot.core.tiers.values().any(|t| independently_ok(&w, t, dc, true));
        let p = &w.snapshot.core.policies[0];
        let risk_block = matches!((p.deny_if_risk_score_above, w.findings.risk_score), (Some(t), Some(r)) if r > t);
        if (!any_ok || risk_block) && p.fallback_tier.is_none() {
            prop_assert_ne!(d.outcome, Outcome::Route, "{}", d.to_json());
        }
        if risk_block {
            prop_assert_eq!(d.outcome, Outcome::Block, "a known risk above the threshold must block: {}", d.to_json());
        }
        if d.outcome == Outcome::Route && !d.fallback {
            let sel = d.estimated_cost_eur.unwrap();
            let sav = d.estimated_savings_eur.unwrap();
            prop_assert!(sav >= 0.0);
            let max_scored = d.candidates.iter().filter(|c| c.score.is_some()).filter_map(|c| c.estimated_cost_eur).fold(0.0f64, f64::max);
            prop_assert!((max_scored - sel - sav).abs() < 1e-6, "savings must be max scored cost minus selected: {}", d.to_json());
        }
    }

    #[test]
    fn deterministic(w in arb_world()) {
        prop_assert_eq!(run(&w).to_json(), run(&w).to_json());
    }

    #[test]
    fn explanation_is_complete(w in arb_world()) {
        let d = run(&w);
        if d.outcome == Outcome::Route && !d.fallback {
            let names: BTreeSet<_> = d.candidates.iter().map(|c| c.tier.clone()).collect();
            let expected: BTreeSet<_> = w.snapshot.core.tiers.keys().cloned().collect();
            prop_assert_eq!(names, expected);
            prop_assert_eq!(d.candidates.iter().filter(|c| c.selected).count(), 1);
            for c in &d.candidates {
                prop_assert!(c.eliminated_by.is_some() != c.score.is_some(), "{}", d.to_json());
                if c.selected { prop_assert!(c.eliminated_by.is_none()); }
            }
        }
        if d.outcome == Outcome::Block {
            prop_assert!(d.selected_tier.is_none());
            prop_assert!(d.reason.is_some());
        }
    }

    #[test]
    fn hints_only_restrict(w in arb_world()) {
        let hinted = run(&w);
        let mut unhinted_w = w.clone();
        unhinted_w.input.hints.data_classes.clear();
        unhinted_w.input.hints.policy = None;
        let unhinted = run(&unhinted_w);
        let survivors = |d: &Decision| -> BTreeSet<String> {
            d.candidates.iter().filter(|c| c.eliminated_by.is_none_or(|r| !r.is_hard())).map(|c| c.tier.clone()).collect()
        };
        if !hinted.fallback && !unhinted.fallback {
            prop_assert!(survivors(&hinted).is_subset(&survivors(&unhinted)), "hint expanded candidates");
        }
        let rank = |d: &Decision| d.data_class.as_ref().and_then(|n| w.snapshot.data_class(n)).map_or(0, |c| c.rank);
        prop_assert!(rank(&hinted) >= rank(&unhinted));
        if unhinted.outcome == Outcome::Block && w.snapshot.core.policies[0].fallback_tier.is_none() {
            prop_assert_eq!(hinted.outcome, Outcome::Block);
        }
    }
}
