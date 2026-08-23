// SPDX-License-Identifier: Apache-2.0
//! `routedctl validate`.

use std::path::PathBuf;

use routed_policy::{CompileReport, compile};
use routed_snapshot::Snapshot;

/// Outcome of a validation run.
#[derive(Debug)]
pub struct Validation {
    /// Diagnostics.
    pub report: CompileReport,
    /// Snapshot hash when compilation succeeded.
    pub hash: Option<String>,
    /// The compiled snapshot itself (for `--emit-snapshot`; the trainer
    /// consumes this instead of re-deriving tier features, ADR-0018).
    pub snapshot: Option<Snapshot>,
}

/// Validate resources offline with the operator's compiler.
///
/// # Errors
/// On unreadable or malformed files (not on compile errors; those are in the report).
pub fn run(paths: &[PathBuf]) -> anyhow::Result<Validation> {
    let input = crate::load_input(paths)?;
    Ok(match compile(&input) {
        Ok((snap, report)) => Validation {
            report,
            hash: Some(snap.hash.clone()),
            snapshot: Some(snap),
        },
        Err(e) => Validation {
            report: e.0,
            hash: None,
            snapshot: None,
        },
    })
}
