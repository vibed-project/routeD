// SPDX-License-Identifier: Apache-2.0
//! Criterion benchmark for trend tracking (`cargo bench -p routed-decision`).
#![allow(clippy::unwrap_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use routed_decision::{DecisionContext, Engine, Findings};

#[path = "../tests/fixture/mod.rs"]
mod fixture;

fn bench_decide(c: &mut Criterion) {
    let snapshot = fixture::big_snapshot(50);
    let input = fixture::bench_input();
    let findings = Findings {
        task: Some("code".into()),
        risk_score: Some(0.2),
        ..Default::default()
    };
    let engine = Engine::new();
    let ctx = DecisionContext { id: "bench".into() };
    c.bench_function("decide/50-tiers/personal", |b| {
        b.iter(|| engine.decide(&snapshot, &input, &findings, &ctx));
    });
}

criterion_group!(benches, bench_decide);
criterion_main!(benches);
