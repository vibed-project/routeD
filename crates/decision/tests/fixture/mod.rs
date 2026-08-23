// SPDX-License-Identifier: Apache-2.0
//! Shared large-snapshot fixture for the latency gate and the criterion bench.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use routed_decision::{DecisionInput, RequestHints};
use routed_snapshot::{
    Capability, CompiledDataClass, CompiledLearnedRouter, CompiledMatch, CompiledPolicy,
    CompiledTier, Currency, Jurisdiction, ObjectiveMode, OperatorControl, SCHEMA_VERSION, Snapshot,
    SnapshotCore, Weights,
};

/// A snapshot with `n_tiers` tiers, two data classes and five policies.
pub fn big_snapshot(n_tiers: usize) -> Snapshot {
    let tiers: BTreeMap<String, CompiledTier> = (0..n_tiers)
        .map(|i| {
            let name = format!("tier-{i:03}");
            let t = CompiledTier {
                name: name.clone(),
                gateway_model: format!("model-{i}"),
                provider: Some("p".into()),
                capabilities: if i % 3 == 0 {
                    BTreeSet::from([Capability::Chat, Capability::Tools, Capability::Json])
                } else {
                    BTreeSet::from([Capability::Chat])
                },
                context_window: 32_000 + (i as u64 * 4_000),
                input_micro_eur_per_million: 100_000 + (i as u64 * 50_000),
                output_micro_eur_per_million: 300_000 + (i as u64 * 150_000),
                currency: Currency::Eur,
                quality_baseline: 0.5 + (i as f64 % 50.0) / 100.0,
                quality_by_task: BTreeMap::from([(
                    "code".to_string(),
                    0.6 + (i as f64 % 40.0) / 100.0,
                )]),
                latency_p50_ms: 200 + (i as u64 * 17) % 900,
                latency_p95_ms: 1000,
                jurisdiction: if i % 2 == 0 {
                    Jurisdiction::Eu
                } else {
                    Jurisdiction::Us
                },
                data_residency: None,
                operator_control: if i % 2 == 0 {
                    OperatorControl::EuEntity
                } else {
                    OperatorControl::UsEntity
                },
                cloud_act_exposed: i % 2 == 1,
                allowed_data_classes: if i % 2 == 0 {
                    BTreeSet::from(["public".to_string(), "personal".to_string()])
                } else {
                    BTreeSet::from(["public".to_string()])
                },
                max_risk_score: 0.9,
                tool_calling_allowed: true,
                labels: BTreeMap::new(),
            };
            (name, t)
        })
        .collect();
    let personal = CompiledDataClass {
        name: "personal".into(),
        rank: 3,
        header_values: BTreeSet::from(["personal".to_string()]),
        pii_entities: BTreeSet::new(),
        min_confidence: 0.7,
        require_jurisdiction: BTreeSet::from([Jurisdiction::Eu]),
        forbid_cloud_act_exposed: true,
        require_operator_control: BTreeSet::from([OperatorControl::EuEntity]),
        forbid_capabilities: BTreeSet::new(),
        max_retention_days: None,
    };
    let public = CompiledDataClass {
        name: "public".into(),
        rank: 0,
        header_values: BTreeSet::from(["public".to_string()]),
        ..personal.clone()
    };
    let policies = (0..5)
        .map(|i| CompiledPolicy {
            key: format!("ns/p{i}"),
            namespace: "ns".into(),
            name: format!("p{i}"),
            priority: 100 - i,
            match_: CompiledMatch {
                tenants: vec![format!("t{i}")],
                agents: vec![],
                paths: vec!["/v1/chat/completions".into()],
                model_aliases: vec!["auto".into(), "routed/*".into()],
            },
            candidates: tiers.keys().cloned().collect(),
            respect_data_class: true,
            max_cost_micro_eur: Some(50_000),
            require_capabilities: BTreeSet::new(),
            deny_if_risk_score_above: Some(0.95),
            mode: ObjectiveMode::CostFirstWithQualityFloor,
            quality_floor: Some(0.75),
            weights: Weights::for_mode(ObjectiveMode::CostFirstWithQualityFloor),
            learned_router: CompiledLearnedRouter::default(),
            reasoning_enabled: true,
            reasoning_map: BTreeMap::new(),
            fallback_tier: Some("tier-000".into()),
            explain: true,
            overridable: false,
        })
        .collect();
    Snapshot::from_core(SnapshotCore {
        schema_version: SCHEMA_VERSION,
        compiler_version: "bench".into(),
        tiers,
        data_classes: BTreeMap::from([
            ("public".to_string(), public),
            ("personal".to_string(), personal),
        ]),
        policies,
        profiles: BTreeMap::new(),
    })
}

/// The request used by the gate and the bench.
pub fn bench_input() -> DecisionInput {
    DecisionInput {
        tenant: Some("t4".into()),
        agent: None,
        path: "/v1/chat/completions".into(),
        requested_model: "auto".into(),
        tools_present: true,
        estimated_input_tokens: 3_000,
        estimated_output_tokens: 256,
        hints: RequestHints {
            data_classes: vec!["personal".into()],
            policy: None,
            dry_run: false,
        },
    }
}
