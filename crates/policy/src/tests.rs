// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use super::*;
use crate::load::{into_input, parse_documents};

const SAMPLE: &str = r#"
apiVersion: routed.io/v1alpha1
kind: DataClass
metadata: { name: public, namespace: ai }
spec: { rank: 0 }
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
kind: ModelTier
metadata: { name: eu-large, namespace: ai }
spec:
  gatewayModel: mistral-large-eu
  capabilities: [chat, tools, json]
  contextWindow: 128000
  cost: { inputPerMillion: 2.0, outputPerMillion: 6.0, currency: EUR }
  quality: { baseline: 0.82, byTask: { code: 0.85 } }
  latency: { p50Ms: 900, p95Ms: 2500 }
  sovereignty: { jurisdiction: EU, operatorControl: eu-entity, cloudActExposed: false, allowedDataClasses: [public, personal] }
---
apiVersion: routed.io/v1alpha1
kind: ModelTier
metadata: { name: us-cheap, namespace: ai }
spec:
  gatewayModel: gpt-mini
  capabilities: [chat, tools]
  contextWindow: 128000
  cost: { inputPerMillion: 0.15, outputPerMillion: 0.6, currency: USD }
  quality: { baseline: 0.7 }
  sovereignty: { jurisdiction: US, operatorControl: us-entity, cloudActExposed: true, allowedDataClasses: [public] }
  labels: { cheap: "true" }
---
apiVersion: routed.io/v1alpha1
kind: RouterProfile
metadata: { name: default, namespace: ai }
spec:
  classifier: { type: heuristic, timeoutMs: 25 }
  costModel: { fxToEUR: { USD: 0.9 } }
---
apiVersion: routed.io/v1alpha1
kind: RoutingPolicy
metadata: { name: default, namespace: ai }
spec:
  priority: 100
  match: { tenants: ["*"], paths: ["/v1/chat/completions"], modelAliases: ["auto", "routed/*"] }
  hardConstraints: { respectDataClass: true, maxCostPerRequestEUR: 0.05 }
  objective: { mode: cost-first-with-quality-floor, qualityFloor: 0.75 }
  fallbackDecision: { tier: eu-large }
"#;

fn sample_input() -> CompileInput {
    into_input(parse_documents(SAMPLE).unwrap())
}

#[test]
fn compiles_sample() {
    let (snap, report) = compile(&sample_input()).unwrap();
    assert!(snap.verify());
    assert_eq!(report.errors().count(), 0);
    assert_eq!(snap.core.policies.len(), 1);
    let p = &snap.core.policies[0];
    assert_eq!(
        p.candidates,
        vec!["eu-large".to_string(), "us-cheap".to_string()]
    );
    assert_eq!(p.max_cost_micro_eur, Some(50_000));
    let us = snap.tier("us-cheap").unwrap();
    assert_eq!(us.input_micro_eur_per_million, 135_000); // 0.15 USD * 0.9 * 1e6
    assert_eq!(snap.core.schema_version, SCHEMA_VERSION);
}

#[test]
fn hash_is_order_independent() {
    let a = compile(&sample_input()).unwrap().0;
    let mut input = sample_input();
    input.tiers.reverse();
    input.data_classes.reverse();
    let b = compile(&input).unwrap().0;
    assert_eq!(a.hash, b.hash);
}

#[test]
fn missing_fx_rate_is_error() {
    let mut input = sample_input();
    input.profiles.clear();
    let err = compile(&input).unwrap_err();
    assert!(
        err.0.errors().any(|d| d.field == "spec.cost.currency"),
        "{}",
        err.0
    );
}

#[test]
fn fallback_tier_that_cannot_serve_a_class_warns() {
    let mut input = sample_input();
    input.policies[0].spec.fallback_decision.tier = Some("us-cheap".into());
    let (_, report) = compile(&input).unwrap();
    assert!(
        report
            .warnings()
            .any(|d| d.field == "spec.fallbackDecision.tier"),
        "{report}"
    );
}

#[test]
fn tier_claiming_a_class_it_violates_warns() {
    let mut input = sample_input();
    // us-cheap is US and CLOUD Act exposed; claiming "personal" is inconsistent.
    input.tiers[1]
        .spec
        .sovereignty
        .allowed_data_classes
        .push("personal".into());
    let (_, report) = compile(&input).unwrap();
    assert!(
        report
            .warnings()
            .any(|d| d.field == "spec.sovereignty.allowedDataClasses"
                && d.message.contains("jurisdiction")),
        "{report}"
    );
}

#[test]
fn unknown_include_is_error_and_selector_filters() {
    let mut input = sample_input();
    input.policies[0].spec.candidates.include = vec!["nope".into()];
    assert!(compile(&input).is_err());
    let mut input = sample_input();
    input.policies[0]
        .spec
        .candidates
        .tier_selector
        .match_labels
        .insert("cheap".into(), "true".into());
    let (snap, _) = compile(&input).unwrap();
    assert_eq!(
        snap.core.policies[0].candidates,
        vec!["us-cheap".to_string()]
    );
}

#[test]
fn shadowed_policy_warns_and_order_is_priority_desc() {
    let mut input = sample_input();
    let mut dup = input.policies[0].clone();
    dup.metadata.name = Some("zzz".into());
    input.policies.push(dup);
    let mut low = input.policies[0].clone();
    low.metadata.name = Some("aaa-low".into());
    low.spec.priority = 1;
    input.policies.push(low);
    let (snap, report) = compile(&input).unwrap();
    let keys: Vec<_> = snap.core.policies.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["ai/default", "ai/zzz", "ai/aaa-low"]);
    assert!(
        report
            .warnings()
            .any(|d| d.name == "ai/zzz" && d.field == "spec.priority")
    );
}

#[test]
fn parse_rejects_unknown_kind() {
    let err = parse_documents("apiVersion: routed.io/v1alpha1\nkind: Nope\nmetadata: {name: x}\n")
        .unwrap_err();
    assert!(matches!(err, load::LoadError::Unsupported { .. }));
}

#[test]
fn data_class_violation_reasons() {
    let (snap, _) = compile(&sample_input()).unwrap();
    let personal = snap.data_class("personal").unwrap();
    assert_eq!(
        data_class_violation(snap.tier("eu-large").unwrap(), personal),
        None
    );
    assert_eq!(
        data_class_violation(snap.tier("us-cheap").unwrap(), personal),
        Some("not in tier.sovereignty.allowedDataClasses")
    );
}

#[test]
fn duplicate_header_values_across_classes_are_rejected() {
    let mut input = sample_input();
    input.data_classes[0].spec.detection.header_values = vec!["pii".into()];
    let err = compile(&input).unwrap_err();
    assert!(
        err.0
            .errors()
            .any(|d| d.field == "spec.detection.headerValues"),
        "{}",
        err.0
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let text = "apiVersion: routed.io/v1alpha1\nkind: RoutingPolicy\nmetadata: {name: p, namespace: ai}\nspec: {priority: 1, hardConstraints: {denyIfRiskAbove: 0.9}}\n";
    assert!(parse_documents(text).is_err());
}

#[test]
fn block_scalars_containing_separators_parse() {
    let text = "apiVersion: routed.io/v1alpha1\nkind: DataClass\nmetadata: {name: x, namespace: ai}\nspec:\n  rank: 1\n  description: |\n    first\n    ---\n    second\n";
    let docs = parse_documents(text).unwrap();
    assert_eq!(docs.len(), 1);
}
