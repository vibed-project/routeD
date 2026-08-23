// SPDX-License-Identifier: Apache-2.0
//! Golden tests: every `examples/NNN-*/` directory is run through the full
//! offline pipeline and compared with `expected.decision.json`.
//! Set `UPDATE_GOLDEN=1` to rewrite the expectations.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use routed_decision::EliminationReason;
use routedctl::explain::{ExplainRequest, run};
use strum::IntoEnumIterator;

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn example_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(examples_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .chars()
                    .next()
                    .unwrap()
                    .is_ascii_digit()
        })
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn goldens_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut failures = Vec::new();
    let mut seen_reasons: BTreeSet<String> = BTreeSet::new();
    let dirs = example_dirs();
    assert!(
        dirs.len() >= 20,
        "expected at least 20 examples, found {}",
        dirs.len()
    );
    for dir in &dirs {
        let req = ExplainRequest::from_dir(dir, "01JGOLDEN0000000000000000")
            .unwrap_or_else(|e| panic!("{}: {e:#}", dir.display()));
        let e = run(&req).unwrap_or_else(|e| panic!("{}: {e:#}", dir.display()));
        for c in &e.decision.candidates {
            if let Some(r) = c.eliminated_by {
                seen_reasons.insert(r.to_string());
            }
        }
        let actual = serde_json::to_string_pretty(&e.decision).unwrap() + "\n";
        let expected_path = dir.join("expected.decision.json");
        if update {
            std::fs::write(&expected_path, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
        if expected != actual {
            failures.push(format!(
                "{}:\n--- expected\n{expected}\n--- actual\n{actual}",
                dir.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches (run with UPDATE_GOLDEN=1 to accept):\n{}",
        failures.join("\n")
    );
    let all: BTreeSet<String> = EliminationReason::iter().map(|r| r.to_string()).collect();
    let missing: Vec<_> = all.difference(&seen_reasons).collect();
    assert!(
        missing.is_empty(),
        "elimination reasons without a golden example: {missing:?}"
    );
}
