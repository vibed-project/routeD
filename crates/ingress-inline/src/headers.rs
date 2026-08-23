// SPDX-License-Identifier: Apache-2.0
//! `X-Routed-*` response headers.

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use routed_decision::Decision;
use serde_json::json;

/// Header names emitted by routeD.
pub const DECISION_ID: HeaderName = HeaderName::from_static("x-routed-decision-id");
/// Selected tier.
pub const TIER: HeaderName = HeaderName::from_static("x-routed-tier");
/// Effective data class.
pub const DATA_CLASS: HeaderName = HeaderName::from_static("x-routed-data-class");
/// Outcome.
pub const OUTCOME: HeaderName = HeaderName::from_static("x-routed-outcome");
/// Compact explanation (base64 JSON).
pub const DECISION: HeaderName = HeaderName::from_static("x-routed-decision");
/// Estimated cost in EUR.
pub const ESTIMATED_COST: HeaderName = HeaderName::from_static("x-routed-estimated-cost");

fn hv(s: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(s).ok()
}

/// Insert the decision headers. The full decision is base64-encoded when it
/// fits `max`; otherwise a compact subset is sent (the full document lives on
/// the span).
pub fn apply(headers: &mut HeaderMap, d: &Decision, explain: bool, max: usize) {
    if let Some(v) = hv(&d.id) {
        headers.insert(DECISION_ID, v);
    }
    if let Some(v) = hv(&d.outcome.to_string()) {
        headers.insert(OUTCOME, v);
    }
    if let Some(v) = d.selected_tier.as_deref().and_then(hv) {
        headers.insert(TIER, v);
    }
    if let Some(v) = d.data_class.as_deref().and_then(hv) {
        headers.insert(DATA_CLASS, v);
    }
    if let Some(v) = d.estimated_cost_eur.and_then(|c| hv(&format!("{c:.8}"))) {
        headers.insert(ESTIMATED_COST, v);
    }
    if explain {
        let full = STANDARD.encode(d.to_json());
        let encoded = if full.len() <= max {
            full
        } else {
            let compact = json!({
                "id": d.id, "outcome": d.outcome, "policy": d.policy, "selectedTier": d.selected_tier,
                "dataClass": d.data_class, "estimatedCostEUR": d.estimated_cost_eur,
                "snapshotHash": d.snapshot_hash, "truncated": true,
            });
            STANDARD.encode(compact.to_string())
        };
        if let Some(v) = hv(&encoded) {
            headers.insert(DECISION, v);
        }
    }
}
