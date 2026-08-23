// SPDX-License-Identifier: Apache-2.0
//! The learned router feature vector (ADR-0018, `routed-features/1`): the
//! runtime and the trainer must agree on this layout byte for byte, so it
//! lives in one place and is deliberately small, deterministic and
//! embedding-free. Tiers are described by features, not identity, so a
//! model survives tier renames and additions.

use routed_decision::{Complexity, Findings};
use routed_snapshot::CompiledTier;

/// Layout version; bump on any change (breaking for trained models).
pub const FEATURES_VERSION: &str = "routed-features/1";

/// Feature vector length for a profile with `task_count` task labels.
#[must_use]
pub fn feature_len(task_count: usize) -> usize {
    task_count + 3 + 1 + 3
}

/// Build the `routed-features/1` vector.
///
/// Layout: task one-hot over `task_labels` (unknown task = all zeros),
/// complexity one-hot (low, medium, high), risk score (0 when absent), tier
/// quality prior for the task, `log10(1 + input+output micro-EUR per MTok)`,
/// latency p50 in seconds.
#[must_use]
pub fn router_features(
    task_labels: &[String],
    findings: &Findings,
    tier: &CompiledTier,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(feature_len(task_labels.len()));
    for label in task_labels {
        out.push(f32::from(findings.task.as_deref() == Some(label.as_str())));
    }
    for c in [Complexity::Low, Complexity::Medium, Complexity::High] {
        out.push(f32::from(findings.complexity == Some(c)));
    }
    #[allow(clippy::cast_possible_truncation)]
    {
        out.push(findings.risk_score.unwrap_or(0.0).clamp(0.0, 1.0) as f32);
        out.push(tier.quality_for(findings.task.as_deref()) as f32);
        let cost = tier.input_micro_eur_per_million + tier.output_micro_eur_per_million;
        out.push(((1.0 + cost as f64).log10()) as f32);
        out.push(tier.latency_p50_ms as f32 / 1000.0);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tier() -> CompiledTier {
        serde_json::from_value(serde_json::json!({
            "name": "t", "gatewayModel": "m", "provider": null,
            "capabilities": [], "contextWindow": 100_000,
            "inputMicroEurPerMillion": 150_000, "outputMicroEurPerMillion": 450_000,
            "currency": "EUR", "qualityBaseline": 0.8,
            "qualityByTask": { "code": 0.9 },
            "latencyP50Ms": 350, "latencyP95Ms": 900,
            "jurisdiction": "EU", "dataResidency": null,
            "operatorControl": "eu-entity", "cloudActExposed": false,
            "allowedDataClasses": [], "maxRiskScore": 1.0,
            "toolCallingAllowed": true, "labels": {}
        }))
        .unwrap()
    }

    #[test]
    fn layout_is_stable() {
        let labels = vec!["code".to_owned(), "chat".to_owned()];
        let findings = Findings {
            task: Some("code".into()),
            complexity: Some(Complexity::Medium),
            risk_score: Some(0.25),
            ..Default::default()
        };
        let f = router_features(&labels, &findings, &tier());
        assert_eq!(f.len(), feature_len(2));
        assert_eq!(&f[0..2], &[1.0, 0.0], "task one-hot");
        assert_eq!(&f[2..5], &[0.0, 1.0, 0.0], "complexity one-hot");
        assert!((f[5] - 0.25).abs() < 1e-6, "risk");
        assert!((f[6] - 0.9).abs() < 1e-6, "quality prior uses the task");
        assert!((f[7] - (600_001f32).log10()).abs() < 1e-3, "log cost");
        assert!((f[8] - 0.35).abs() < 1e-6, "latency seconds");
    }

    #[test]
    fn unknown_task_is_all_zeros_and_baseline_quality() {
        let labels = vec!["code".to_owned()];
        let findings = Findings::default();
        let f = router_features(&labels, &findings, &tier());
        assert!(f[0].abs() < f32::EPSILON);
        assert!((f[5] - 0.8).abs() < 1e-6, "baseline quality prior");
    }
}
