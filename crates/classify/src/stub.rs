// SPDX-License-Identifier: Apache-2.0
//! Deterministic stub returning configured findings (tests, goldens, e2e).

use routed_decision::Findings;

use crate::{Classifier, ClassifyError, ClassifyInput};

/// Returns fixed findings, or a fixed error.
#[derive(Clone, Debug, Default)]
pub struct StubClassifier {
    /// Findings to return.
    pub findings: Findings,
    /// Error to return instead, if set.
    pub error: Option<ClassifyError>,
}

impl StubClassifier {
    /// Stub returning the given findings.
    #[must_use]
    pub fn returning(findings: Findings) -> Self {
        Self {
            findings,
            error: None,
        }
    }

    /// Stub that always fails with the given error.
    #[must_use]
    pub fn failing(error: ClassifyError) -> Self {
        Self {
            findings: Findings::default(),
            error: Some(error),
        }
    }
}

impl Classifier for StubClassifier {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn classify(&self, _input: &ClassifyInput) -> Result<Findings, ClassifyError> {
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(self.findings.clone()),
        }
    }
}
