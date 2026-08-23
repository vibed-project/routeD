// SPDX-License-Identifier: Apache-2.0
//! Criterion benchmark for trend tracking (`cargo bench -p routed-classify`).
//! With `--features onnx` and `ORT_DYLIB_PATH` set, also benches the ONNX
//! pipeline against the committed fixture model.
#![allow(clippy::unwrap_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use routed_classify::{Classifier, ClassifyInput, HeuristicClassifier};

fn bench_classify(c: &mut Criterion) {
    let short = ClassifyInput::user("Summarise the quarterly report in three bullets.");
    let long = ClassifyInput {
        user_text: "Please analyse the following logs and explain why the deploy failed. "
            .repeat(200),
        system_prompt: Some("You are a helpful SRE assistant.".into()),
        tool_outputs: vec!["error: connection refused (upstream 10.0.0.7:5432)".repeat(50)],
        ..Default::default()
    };

    let h = HeuristicClassifier::default();
    c.bench_function("classify/heuristic/short", |b| {
        b.iter(|| h.classify(&short).unwrap());
    });
    c.bench_function("classify/heuristic/long", |b| {
        b.iter(|| h.classify(&long).unwrap());
    });

    #[cfg(feature = "onnx")]
    {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let o = routed_classify::onnx::OnnxClassifier::load(
            &dir.join("model.onnx"),
            &dir.join("tokenizer.json"),
            vec!["code".into(), "chat".into(), "reasoning".into()],
            vec!["public".into(), "personal".into()],
        )
        .unwrap();
        c.bench_function("classify/onnx-fixture/short", |b| {
            b.iter(|| o.classify(&short).unwrap());
        });
        c.bench_function("classify/onnx-fixture/long", |b| {
            b.iter(|| o.classify(&long).unwrap());
        });
    }
}

criterion_group!(benches, bench_classify);
criterion_main!(benches);
