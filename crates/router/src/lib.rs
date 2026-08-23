// SPDX-License-Identifier: Apache-2.0
//! Request pipeline glue: context extraction, classification, decision,
//! parameter computation.
//!
//! The only crate that wires classify, security, decision, cache and feedback
//! together. Everything here is still synchronous and I/O free; the ingress
//! crates add the servers, timeouts and telemetry emission.

pub mod extract;

use routed_classify::{Classifier, ClassifyError};
use routed_decision::{
    Decision, DecisionContext, DecisionInput, Engine, Findings, QualityPredictor,
};
use routed_security::{RequestHeaders, extract_headers};
use routed_snapshot::Snapshot;

pub use extract::{ParsedRequest, parse_request};

/// Everything needed to decide once, synchronously. Used by `routedctl explain`
/// and by the ingress layers (which add timeouts around `classify`).
pub struct Pipeline<'a, P: QualityPredictor = routed_decision::NoPredictor> {
    /// Engine.
    pub engine: &'a Engine<P>,
    /// Snapshot to decide against.
    pub snapshot: &'a Snapshot,
    /// Classifier.
    pub classifier: &'a dyn Classifier,
}

/// Result of [`Pipeline::run`].
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The decision.
    pub decision: Decision,
    /// The input the engine saw (for explanations).
    pub input: DecisionInput,
    /// The findings the engine saw.
    pub findings: Findings,
    /// Parsed headers (ignored ones are reported back for telemetry).
    pub headers: RequestHeaders,
}

impl<P: QualityPredictor> Pipeline<'_, P> {
    /// Decide for a raw OpenAI-format request body and its headers.
    ///
    /// `findings_override` replaces classification (tests / explain). A
    /// classifier error is mapped to a degraded finding so the engine applies
    /// the policy fallback rather than guessing.
    ///
    /// # Errors
    /// When the body is not a request routeD understands.
    pub fn run<'h, I, N, V>(
        &self,
        path: &str,
        headers: I,
        body: &[u8],
        findings_override: Option<Findings>,
        ctx: &DecisionContext,
    ) -> Result<Outcome, extract::ParseError>
    where
        I: IntoIterator<Item = (N, V)>,
        N: AsRef<str> + 'h,
        V: AsRef<str> + 'h,
    {
        let headers = extract_headers(headers);
        let parsed = parse_request(path, body)?;
        let default_output = self
            .snapshot
            .core
            .profiles
            .get("default")
            .or_else(|| self.snapshot.core.profiles.values().next())
            .map_or(256, |p| p.default_output_tokens);
        let input = parsed.to_input(path, &headers, default_output);
        let findings = match findings_override {
            Some(f) => f,
            None => match self.classifier.classify(&parsed.classify_input) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(classifier = self.classifier.name(), error = %e, "classification failed; applying fallback");
                    Findings {
                        degraded: vec![degraded_name(self.classifier.name(), &e)],
                        ..Default::default()
                    }
                }
            },
        };
        let decision = self.engine.decide(self.snapshot, &input, &findings, ctx);
        Ok(Outcome {
            decision,
            input,
            findings,
            headers,
        })
    }
}

/// Name recorded in `Findings::degraded` for a classifier failure.
#[must_use]
pub fn degraded_name(classifier: &str, e: &ClassifyError) -> String {
    match e {
        ClassifyError::Timeout(_) => format!("{classifier}:timeout"),
        ClassifyError::Unavailable(_) => format!("{classifier}:unavailable"),
        ClassifyError::Failed(_) => format!("{classifier}:failed"),
    }
}
