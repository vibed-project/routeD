// SPDX-License-Identifier: Apache-2.0
//! Engine latency gate: p95 of a single decision over a 50-tier snapshot must
//! stay under 1 ms. Runs only with `ROUTED_PERF=1` (see docs/performance.md);
//! `ROUTED_PERF_SLACK` multiplies the budget (for noisy VMs).
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod fixture;

use fixture::{bench_input, big_snapshot};

use std::time::{Duration, Instant};

use routed_decision::{DecisionContext, Engine, Findings, Outcome};

#[test]
fn engine_p95_under_one_millisecond() {
    if std::env::var_os("ROUTED_PERF").is_none() {
        eprintln!("skipped: set ROUTED_PERF=1 to run the latency gate");
        return;
    }
    let slack: f64 = std::env::var("ROUTED_PERF_SLACK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    let snapshot = big_snapshot(50);
    let input = bench_input();
    let findings = Findings {
        task: Some("code".into()),
        risk_score: Some(0.2),
        ..Default::default()
    };
    let engine = Engine::new();
    let ctx = DecisionContext { id: "perf".into() };
    for _ in 0..1_000 {
        let _ = engine.decide(&snapshot, &input, &findings, &ctx);
    }
    let mut samples: Vec<Duration> = Vec::with_capacity(20_000);
    for _ in 0..20_000 {
        let t = Instant::now();
        let d = engine.decide(&snapshot, &input, &findings, &ctx);
        samples.push(t.elapsed());
        assert_eq!(d.outcome, Outcome::Route);
    }
    samples.sort();
    let p = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
    let (p50, p95, p99) = (p(0.50), p(0.95), p(0.99));
    eprintln!("engine latency over 50 tiers: p50={p50:?} p95={p95:?} p99={p99:?} (slack x{slack})");
    let budget = Duration::from_secs_f64(0.001 * slack);
    assert!(p95 < budget, "p95 {p95:?} exceeds budget {budget:?}");
}
