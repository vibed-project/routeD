// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use std::collections::BTreeSet;

use routed_policy::load::{into_input, parse_documents};
use routed_snapshot::{ObjectiveMode, Snapshot};
use strum::IntoEnumIterator;

use super::*;

const FIXTURE: &str = r#"
apiVersion: routed.io/v1alpha1
kind: DataClass
metadata: { name: public, namespace: ai }
spec: { rank: 0, detection: { headerValues: [public] } }
---
apiVersion: routed.io/v1alpha1
kind: DataClass
metadata: { name: internal, namespace: ai }
spec: { rank: 1, detection: { headerValues: [internal] } }
---
apiVersion: routed.io/v1alpha1
kind: DataClass
metadata: { name: personal, namespace: ai }
spec:
  rank: 3
  detection: { headerValues: [personal, pii], piiEntities: [EMAIL, IBAN] }
  constraints: { requireJurisdiction: [EU], forbidCloudActExposed: true, requireOperatorControl: [eu-entity, on-prem] }
---
apiVersion: routed.io/v1alpha1
kind: DataClass
metadata: { name: restricted, namespace: ai }
spec:
  rank: 5
  detection: { headerValues: [restricted] }
  constraints: { requireJurisdiction: [EU], forbidCapabilities: [tools] }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: eu-large, namespace: ai }
spec:
  gatewayModel: mistral-large-eu
  capabilities: [chat, tools, json]
  contextWindow: 128000
  cost: { inputPerMillion: 2.0, outputPerMillion: 6.0, currency: EUR }
  quality: { baseline: 0.82, byTask: { code: 0.85 } }
  latency: { p50Ms: 900, p95Ms: 2500 }
  sovereignty: { jurisdiction: EU, operatorControl: eu-entity, cloudActExposed: false, allowedDataClasses: [public, internal, personal, restricted] }
  security: { maxRiskScore: 0.9 }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: eu-small, namespace: ai }
spec:
  gatewayModel: mistral-small-eu
  capabilities: [chat, json]
  contextWindow: 32000
  cost: { inputPerMillion: 0.2, outputPerMillion: 0.6, currency: EUR }
  quality: { baseline: 0.7 }
  latency: { p50Ms: 300, p95Ms: 900 }
  sovereignty: { jurisdiction: EU, operatorControl: eu-entity, cloudActExposed: false, allowedDataClasses: [public, internal, personal, restricted] }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: us-cheap, namespace: ai }
spec:
  gatewayModel: gpt-mini
  capabilities: [chat, tools]
  contextWindow: 128000
  cost: { inputPerMillion: 0.15, outputPerMillion: 0.6, currency: USD }
  quality: { baseline: 0.75 }
  latency: { p50Ms: 400, p95Ms: 1200 }
  sovereignty: { jurisdiction: US, operatorControl: us-entity, cloudActExposed: true, allowedDataClasses: [public] }
  security: { maxRiskScore: 0.5, toolCallingAllowed: false }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: ch-mid, namespace: ai }
spec:
  gatewayModel: swiss-mid
  capabilities: [chat, tools]
  contextWindow: 128000
  cost: { inputPerMillion: 3.0, outputPerMillion: 9.0, currency: EUR }
  quality: { baseline: 0.8 }
  latency: { p50Ms: 600, p95Ms: 1500 }
  sovereignty: { jurisdiction: OTHER, operatorControl: on-prem, cloudActExposed: false, allowedDataClasses: [public, internal, personal] }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: eu-us-op, namespace: ai }
spec:
  gatewayModel: eu-hosted-us-vendor
  capabilities: [chat, tools]
  contextWindow: 128000
  cost: { inputPerMillion: 3.0, outputPerMillion: 9.0, currency: EUR }
  quality: { baseline: 0.8 }
  latency: { p50Ms: 700, p95Ms: 1500 }
  sovereignty: { jurisdiction: EU, operatorControl: us-entity, cloudActExposed: false, allowedDataClasses: [public, internal, personal] }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: eu-cloudact, namespace: ai }
spec:
  gatewayModel: eu-region-hyperscaler
  capabilities: [chat, tools]
  contextWindow: 128000
  cost: { inputPerMillion: 3.0, outputPerMillion: 9.0, currency: EUR }
  quality: { baseline: 0.8 }
  latency: { p50Ms: 800, p95Ms: 1500 }
  sovereignty: { jurisdiction: EU, operatorControl: eu-entity, cloudActExposed: true, allowedDataClasses: [public, internal, personal] }
---
apiVersion: routed.io/v1alpha1
kind: RouterProfile
metadata: { name: default, namespace: ai }
spec:
  classifier: { type: heuristic }
  costModel: { fxToEUR: { USD: 0.9 } }
---
apiVersion: routed.io/v1alpha1
kind: RoutingPolicy
metadata: { name: default, namespace: ai }
spec:
  priority: 100
  match: { tenants: ["*"], paths: ["/v1/chat/completions"], modelAliases: ["auto", "routed/*"] }
  hardConstraints: { respectDataClass: true, maxCostPerRequestEUR: 0.05, denyIfRiskScoreAbove: 0.95 }
  objective: { mode: cost-first-with-quality-floor, qualityFloor: 0.75 }
  reasoningBudget: { enabled: true, map: { low: none, medium: low, high: high } }
  fallbackDecision: { tier: eu-large }
---
apiVersion: routed.io/v1alpha1
kind: RoutingPolicy
metadata: { name: tenant-b, namespace: ai }
spec:
  priority: 200
  match: { tenants: ["b"], modelAliases: ["auto"] }
  hardConstraints: { requireCapabilities: [vision] }
---
apiVersion: routed.io/v1alpha1
kind: RoutingPolicy
metadata: { name: strict-floor, namespace: ai }
spec:
  priority: 300
  match: { tenants: ["strict"], modelAliases: ["auto"] }
  objective: { mode: cost-first-with-quality-floor, qualityFloor: 0.99 }
---
apiVersion: routed.io/v1alpha1
kind: RoutingPolicy
metadata: { name: fast-lane, namespace: ai }
spec:
  priority: 10
  match: { tenants: ["*"], paths: ["/v1/chat/completions"], modelAliases: ["auto"] }
  candidates: { include: [eu-small] }
  overridable: true
"#;

fn snapshot() -> Snapshot {
    routed_policy::compile(&into_input(parse_documents(FIXTURE).unwrap()))
        .unwrap()
        .0
}

fn input(model: &str) -> DecisionInput {
    DecisionInput {
        tenant: Some("a".into()),
        agent: None,
        path: "/v1/chat/completions".into(),
        requested_model: model.into(),
        tools_present: false,
        estimated_input_tokens: 1000,
        estimated_output_tokens: 256,
        hints: RequestHints::default(),
    }
}

/// Findings with a known (low) risk score; an absent score is treated as degraded.
fn base() -> Findings {
    Findings {
        risk_score: Some(0.0),
        ..Default::default()
    }
}

fn ctx() -> DecisionContext {
    DecisionContext {
        id: "d-000001".into(),
    }
}

fn decide(input: &DecisionInput, findings: &Findings) -> Decision {
    Engine::new().decide(&snapshot(), input, findings, &ctx())
}

fn reason_of(d: &Decision, tier: &str) -> Option<EliminationReason> {
    candidates_by_name(d)
        .get(tier)
        .and_then(|c| c.eliminated_by)
}

#[test]
fn pass_through_when_alias_not_routed() {
    let d = decide(&input("gpt-4o"), &base());
    assert_eq!(d.outcome, Outcome::PassThrough);
    assert_eq!(d.policy.as_deref(), Some("ai/default"));
    assert!(d.selected_tier.is_none());
}

#[test]
fn pass_through_when_no_policy_matches() {
    let mut i = input("auto");
    i.path = "/v1/embeddings".into();
    let d = decide(&i, &base());
    assert_eq!(d.outcome, Outcome::PassThrough);
    assert!(d.policy.is_none());
}

#[test]
fn routes_cheapest_above_quality_floor() {
    let d = decide(&input("auto"), &base());
    assert_eq!(d.outcome, Outcome::Route);
    assert_eq!(d.selected_tier.as_deref(), Some("us-cheap"));
    assert_eq!(d.gateway_model.as_deref(), Some("gpt-mini"));
    assert_eq!(
        reason_of(&d, "eu-small"),
        Some(EliminationReason::QualityFloor)
    );
    assert!(d.estimated_savings_eur.unwrap() > 0.0);
    assert!(d.to_json().contains("\"outcome\":\"ROUTE\""));
    assert!(d.to_json().contains("\"estimatedCostEUR\""));
}

#[test]
fn personal_header_selects_eu_only() {
    let mut i = input("auto");
    i.hints.data_classes = vec!["PII".into()];
    let d = decide(&i, &base());
    assert_eq!(d.data_class.as_deref(), Some("personal"));
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
    assert_eq!(
        reason_of(&d, "us-cheap"),
        Some(EliminationReason::DataClassNotAllowed)
    );
    assert_eq!(
        reason_of(&d, "ch-mid"),
        Some(EliminationReason::DataClassRequireJurisdiction)
    );
    assert_eq!(
        reason_of(&d, "eu-us-op"),
        Some(EliminationReason::DataClassRequireOperatorControl)
    );
    assert_eq!(
        reason_of(&d, "eu-cloudact"),
        Some(EliminationReason::DataClassForbidCloudActExposed)
    );
}

#[test]
fn pii_infers_personal_and_header_cannot_lower_it() {
    let mut i = input("auto");
    i.hints.data_classes = vec!["public".into()];
    let f = Findings {
        pii_entities: BTreeSet::from([PiiEntity::Email]),
        ..Default::default()
    };
    let d = decide(&i, &f);
    assert_eq!(d.data_class.as_deref(), Some("personal"));
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
}

#[test]
fn unknown_header_class_is_ignored_with_note() {
    let mut i = input("auto");
    i.hints.data_classes = vec!["top-secret".into()];
    let d = decide(&i, &base());
    assert!(d.data_class.is_none());
    assert!(d.notes.iter().any(|n| n.contains("unknown data class")));
}

#[test]
fn restricted_class_forbids_tools_capability() {
    let mut i = input("auto");
    i.hints.data_classes = vec!["restricted".into()];
    let d = decide(&i, &base());
    assert_eq!(
        reason_of(&d, "eu-large"),
        Some(EliminationReason::DataClassForbidCapabilities)
    );
    // eu-small is the only survivor; it is below the floor, so it is kept as best available
    assert_eq!(d.selected_tier.as_deref(), Some("eu-small"));
}

#[test]
fn risk_above_policy_threshold_blocks() {
    let f = Findings {
        risk_score: Some(0.96),
        ..Default::default()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(d.outcome, Outcome::Block);
    assert!(
        d.candidates
            .iter()
            .all(|c| c.eliminated_by == Some(EliminationReason::DenyIfRiskScoreAbove))
    );
    assert!(d.selected_tier.is_none());
}

#[test]
fn risk_above_tier_limit_eliminates_tier() {
    let f = Findings {
        risk_score: Some(0.6),
        ..Default::default()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(
        reason_of(&d, "us-cheap"),
        Some(EliminationReason::MaxRiskScore)
    );
    assert_eq!(d.outcome, Outcome::Route);
}

#[test]
fn tools_eliminate_unsupporting_tiers() {
    let mut i = input("auto");
    i.tools_present = true;
    let d = decide(&i, &base());
    assert_eq!(
        reason_of(&d, "eu-small"),
        Some(EliminationReason::ToolsNotSupported)
    );
    assert_eq!(
        reason_of(&d, "us-cheap"),
        Some(EliminationReason::ToolCallingNotAllowed)
    );
}

#[test]
fn context_window_eliminates_small_tier() {
    let mut i = input("auto");
    i.estimated_input_tokens = 100_000;
    let d = decide(&i, &base());
    assert_eq!(
        reason_of(&d, "eu-small"),
        Some(EliminationReason::ContextWindow)
    );
}

#[test]
fn cost_cap_eliminates_expensive_tiers() {
    let mut i = input("auto");
    i.estimated_input_tokens = 30_000;
    let d = decide(&i, &base());
    assert_eq!(
        reason_of(&d, "eu-large"),
        Some(EliminationReason::MaxCostPerRequest)
    );
    assert_eq!(
        reason_of(&d, "ch-mid"),
        Some(EliminationReason::MaxCostPerRequest)
    );
    assert_eq!(d.selected_tier.as_deref(), Some("us-cheap"));
}

#[test]
fn required_capability_eliminates_all_and_blocks_without_fallback() {
    let mut i = input("auto");
    i.tenant = Some("b".into());
    let d = decide(&i, &base());
    assert_eq!(d.policy.as_deref(), Some("ai/tenant-b"));
    assert_eq!(d.outcome, Outcome::Block);
    assert!(
        d.candidates
            .iter()
            .all(|c| c.eliminated_by == Some(EliminationReason::RequireCapabilities))
    );
    assert!(d.fallback);
}

#[test]
fn degraded_classification_uses_fallback_tier() {
    let f = Findings {
        degraded: vec!["classifier".into()],
        ..base()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(d.outcome, Outcome::Route);
    assert!(d.fallback);
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
    assert_eq!(d.degraded, vec!["classifier".to_string()]);
}

#[test]
fn degraded_with_data_class_still_respects_it() {
    let mut i = input("auto");
    i.hints.data_classes = vec!["personal".into()];
    let f = Findings {
        degraded: vec!["classifier".into()],
        ..Default::default()
    };
    let d = decide(&i, &f);
    assert_eq!(d.outcome, Outcome::Route);
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
}

#[test]
fn policy_override_only_to_overridable_matching_policies() {
    let mut i = input("auto");
    i.tenant = Some("b".into());
    // default matches (tenants *) but is not overridable: a header must not loosen vision-only.
    i.hints.policy = Some("default".into());
    let d = decide(&i, &base());
    assert_eq!(d.policy.as_deref(), Some("ai/tenant-b"));
    assert!(d.notes.iter().any(|n| n.contains("not overridable")));
    // strict-floor does not match tenant b at all.
    i.hints.policy = Some("strict-floor".into());
    let d = decide(&i, &base());
    assert_eq!(d.policy.as_deref(), Some("ai/tenant-b"));
    assert!(
        d.notes
            .iter()
            .any(|n| n.contains("ignored X-Routed-Policy"))
    );
    // fast-lane matches and is explicitly overridable.
    i.hints.policy = Some("fast-lane".into());
    let d = decide(&i, &base());
    assert_eq!(d.policy.as_deref(), Some("ai/fast-lane"));
    assert_eq!(d.selected_tier.as_deref(), Some("eu-small"));
}

#[test]
fn fallback_still_respects_hard_constraints_when_nothing_survives() {
    // Everything is eliminated by the cost cap, including the fallback tier: BLOCK, not ROUTE.
    let mut i = input("auto");
    i.estimated_input_tokens = 200_000; // eu-large: 0.4 EUR >> cap; also beyond every context window
    let d = decide(&i, &base());
    assert_eq!(d.outcome, Outcome::Block, "{}", d.to_json());
    assert!(d.fallback);
    assert!(d.reason.as_deref().unwrap().contains("fallback tier"));
}

#[test]
fn degraded_fallback_still_checks_request_facts_and_known_risk() {
    // tools present but fallback eu-large allows tools: routed
    let mut i = input("auto");
    i.tools_present = true;
    let d = decide(
        &i,
        &Findings {
            degraded: vec!["pii".into()],
            ..base()
        },
    );
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
    // known risk above the fallback tier's maxRiskScore (0.9): blocked even though degraded
    let d = decide(
        &input("auto"),
        &Findings {
            degraded: vec!["pii".into()],
            risk_score: Some(0.92),
            ..Default::default()
        },
    );
    assert_eq!(d.outcome, Outcome::Block, "{}", d.to_json());
}

#[test]
fn partial_degradation_does_not_skip_the_risk_block() {
    let f = Findings {
        degraded: vec!["pii".into()],
        risk_score: Some(0.99),
        ..Default::default()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(d.outcome, Outcome::Block);
    assert!(
        d.candidates
            .iter()
            .all(|c| c.eliminated_by == Some(EliminationReason::DenyIfRiskScoreAbove))
    );
}

#[test]
fn missing_risk_score_is_degraded_not_permissive() {
    let d = decide(&input("auto"), &Findings::default());
    assert!(d.fallback);
    assert!(d.degraded.iter().any(|x| x == "risk:missing"));
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
}

#[test]
fn data_class_min_confidence_is_applied() {
    let mut f = base();
    f.pii_entities.insert(PiiEntity::Email);
    f.pii_confidence.insert(PiiEntity::Email, 0.5); // personal requires 0.7 (default)
    let d = decide(&input("auto"), &f);
    assert!(d.data_class.is_none());
    f.pii_confidence.insert(PiiEntity::Email, 0.9);
    let d = decide(&input("auto"), &f);
    assert_eq!(d.data_class.as_deref(), Some("personal"));
}

#[test]
fn quality_floor_unreachable_keeps_best_quality() {
    let mut i = input("auto");
    i.tenant = Some("strict".into());
    let d = decide(&i, &base());
    assert_eq!(d.outcome, Outcome::Route);
    assert_eq!(d.selected_tier.as_deref(), Some("eu-large"));
    assert!(d.reason.as_deref().unwrap().contains("qualityFloor"));
}

#[test]
fn reasoning_parameter_follows_complexity() {
    let f = Findings {
        complexity: Some(Complexity::High),
        ..Default::default()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(d.parameters.reasoning, Some(ReasoningLevel::High));
}

#[test]
fn task_quality_override_is_used() {
    let f = Findings {
        task: Some("code".into()),
        ..Default::default()
    };
    let d = decide(&input("auto"), &f);
    assert_eq!(
        candidates_by_name(&d)["eu-large"].predicted_quality,
        Some(0.85)
    );
}

#[test]
fn every_objective_mode_routes_deterministically() {
    let mut snap = snapshot();
    let mut expected = std::collections::BTreeMap::new();
    expected.insert(ObjectiveMode::CostFirstWithQualityFloor, "us-cheap");
    expected.insert(ObjectiveMode::QualityFirst, "eu-large");
    expected.insert(ObjectiveMode::LatencyFirst, "us-cheap");
    expected.insert(ObjectiveMode::Balanced, "us-cheap");
    for mode in [
        ObjectiveMode::CostFirstWithQualityFloor,
        ObjectiveMode::QualityFirst,
        ObjectiveMode::Balanced,
        ObjectiveMode::LatencyFirst,
    ] {
        snap.core.policies[2].mode = mode; // ai/default (priority 100 sorts last)
        snap.core.policies[2].weights = routed_snapshot::Weights::for_mode(mode);
        let d1 = Engine::new().decide(&snap, &input("auto"), &base(), &ctx());
        let d2 = Engine::new().decide(&snap, &input("auto"), &base(), &ctx());
        assert_eq!(d1, d2, "non-deterministic for {mode:?}");
        assert_eq!(d1.outcome, Outcome::Route, "{mode:?}");
        assert_eq!(
            d1.selected_tier.as_deref(),
            Some(expected[&mode]),
            "{mode:?}"
        );
    }
}

#[test]
fn every_elimination_reason_is_exercised() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut collect = |d: Decision| {
        for c in d.candidates {
            if let Some(r) = c.eliminated_by {
                seen.insert(r.to_string());
            }
        }
    };
    collect(decide(
        &input("auto"),
        &Findings {
            risk_score: Some(0.96),
            ..Default::default()
        },
    ));
    let mut i = input("auto");
    i.hints.data_classes = vec!["personal".into()];
    collect(decide(&i, &base()));
    i.hints.data_classes = vec!["restricted".into()];
    collect(decide(&i, &base()));
    let mut i = input("auto");
    i.tools_present = true;
    collect(decide(&i, &base()));
    let mut i = input("auto");
    i.estimated_input_tokens = 100_000;
    collect(decide(&i, &base()));
    let mut i = input("auto");
    i.estimated_input_tokens = 30_000;
    collect(decide(&i, &base()));
    let mut i = input("auto");
    i.tenant = Some("b".into());
    collect(decide(&i, &base()));
    collect(decide(
        &input("auto"),
        &Findings {
            risk_score: Some(0.6),
            ..Default::default()
        },
    ));
    collect(decide(&input("auto"), &base()));

    let all: BTreeSet<String> = EliminationReason::iter().map(|r| r.to_string()).collect();
    let missing: Vec<_> = all.difference(&seen).collect();
    assert!(
        missing.is_empty(),
        "elimination reasons without a test case: {missing:?}"
    );
}

#[test]
fn elimination_reason_round_trips_through_json() {
    for r in EliminationReason::iter() {
        let json = serde_json::to_string(&r).unwrap();
        let back: EliminationReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }
    assert_eq!(
        serde_json::to_string(&Outcome::PassThrough).unwrap(),
        "\"PASS_THROUGH\""
    );
}

#[test]
fn glob_semantics() {
    assert!(glob_match("*", "anything"));
    assert!(glob_match("routed/*", "routed/fast"));
    assert!(!glob_match("routed/*", "auto"));
    assert!(glob_match("*-eu", "mistral-eu"));
    assert!(glob_match("auto", "auto"));
    assert!(!glob_match("auto", "Auto"));
}
