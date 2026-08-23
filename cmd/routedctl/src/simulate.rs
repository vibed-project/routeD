// SPDX-License-Identifier: Apache-2.0
//! `routedctl simulate` (ADR-0018): replay a JSONL request log against a
//! snapshot with the same offline pipeline as `explain`, and aggregate the
//! outcomes. What-if analysis before a policy change ships.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use routed_decision::{DecisionContext, Engine, Outcome};
use routed_policy::compile;
use routed_router::Pipeline;
use serde::{Deserialize, Serialize};

/// One line of the requests file: either a bare OpenAI-format request, or a
/// wrapper with path and headers.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LineIn {
    Wrapped {
        request: serde_json::Value,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Bare(serde_json::Value),
}

/// Aggregated simulation result.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Requests replayed.
    pub requests: usize,
    /// Lines that failed to parse or decide (counted, not fatal).
    pub errors: usize,
    /// Decisions per outcome.
    pub outcomes: BTreeMap<String, usize>,
    /// `ROUTE` decisions per selected tier.
    pub tiers: BTreeMap<String, usize>,
    /// Decisions per effective data class.
    pub data_classes: BTreeMap<String, usize>,
    /// `BLOCK` decisions per reason.
    pub block_reasons: BTreeMap<String, usize>,
    /// Sum of estimated cost (EUR) over routed requests.
    pub total_estimated_cost_eur: f64,
    /// Sum of estimated savings (EUR) versus the most expensive candidate.
    pub total_estimated_savings_eur: f64,
    /// Decisions that used the policy fallback.
    pub fallbacks: usize,
    /// Decisions with degraded classifiers.
    pub degraded: usize,
}

/// Replay `requests` (JSONL) against the resources in `policy`.
///
/// # Errors
/// On unreadable inputs or a non-compiling policy set; individual bad lines
/// are counted in [`Summary::errors`] instead.
pub fn run(policy: &PathBuf, requests: &PathBuf) -> anyhow::Result<Summary> {
    let input = crate::load_input(std::slice::from_ref(policy))?;
    let (snapshot, _report) =
        compile(&input).map_err(|e| anyhow::anyhow!("resources do not compile:\n{}", e.0))?;
    let engine = Engine::new();
    let classifier = routed_classify::from_profile(
        snapshot
            .core
            .profiles
            .get("default")
            .or_else(|| snapshot.core.profiles.values().next()),
    )
    .map_err(|e| anyhow::anyhow!("{e}; use a heuristic/stub RouterProfile for simulation"))?;
    let pipeline = Pipeline {
        engine: &engine,
        snapshot: &snapshot,
        classifier: classifier.as_ref(),
    };

    let text = std::fs::read_to_string(requests)
        .with_context(|| format!("reading {}", requests.display()))?;
    let mut summary = Summary::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        summary.requests += 1;
        let (request, path, headers) = match serde_json::from_str::<LineIn>(line) {
            Ok(LineIn::Wrapped {
                request,
                path,
                headers,
            }) => (
                request,
                path.unwrap_or_else(|| "/v1/chat/completions".to_owned()),
                headers,
            ),
            Ok(LineIn::Bare(request)) => {
                (request, "/v1/chat/completions".to_owned(), BTreeMap::new())
            }
            Err(e) => {
                eprintln!("line {}: unparseable: {e}", i + 1);
                summary.errors += 1;
                continue;
            }
        };
        let body = request.to_string().into_bytes();
        let ctx = DecisionContext {
            id: format!("SIM{:023}", i + 1),
        };
        let out = match pipeline.run(&path, headers.iter(), &body, None, &ctx) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("line {}: did not decide: {e}", i + 1);
                summary.errors += 1;
                continue;
            }
        };
        let d = &out.decision;
        *summary.outcomes.entry(d.outcome.to_string()).or_default() += 1;
        if let Some(t) = &d.selected_tier {
            *summary.tiers.entry(t.clone()).or_default() += 1;
        }
        if let Some(dc) = &d.data_class {
            *summary.data_classes.entry(dc.clone()).or_default() += 1;
        }
        if d.outcome == Outcome::Block {
            let reason = d.reason.clone().unwrap_or_else(|| "unspecified".into());
            *summary.block_reasons.entry(reason).or_default() += 1;
        }
        summary.total_estimated_cost_eur += d.estimated_cost_eur.unwrap_or(0.0);
        summary.total_estimated_savings_eur += d.estimated_savings_eur.unwrap_or(0.0);
        summary.fallbacks += usize::from(d.fallback);
        summary.degraded += usize::from(!d.degraded.is_empty());
    }
    summary.total_estimated_cost_eur = round6(summary.total_estimated_cost_eur);
    summary.total_estimated_savings_eur = round6(summary.total_estimated_savings_eur);
    Ok(summary)
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// Human-readable rendering.
#[must_use]
pub fn render(s: &Summary) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "requests: {} ({} errors)", s.requests, s.errors);
    let section = |out: &mut String, name: &str, map: &BTreeMap<String, usize>| {
        if map.is_empty() {
            return;
        }
        let _ = writeln!(out, "{name}:");
        for (k, v) in map {
            let _ = writeln!(out, "  {k}: {v}");
        }
    };
    section(&mut out, "outcomes", &s.outcomes);
    section(&mut out, "tiers", &s.tiers);
    section(&mut out, "data classes", &s.data_classes);
    section(&mut out, "block reasons", &s.block_reasons);
    let _ = writeln!(
        out,
        "estimated cost: {:.6} EUR (savings {:.6} EUR)",
        s.total_estimated_cost_eur, s.total_estimated_savings_eur
    );
    if s.fallbacks > 0 || s.degraded > 0 {
        let _ = writeln!(out, "fallbacks: {}  degraded: {}", s.fallbacks, s.degraded);
    }
    out
}
