# ADR-0006: Classifier seam and degradation semantics

## Status

Accepted

## Context

Classification (task, complexity, sensitivity / PII, injection risk) must be
pluggable: heuristics before any model exists, ONNX models in-process later,
external HTTP services for customers who bring their own. It must also fail
safe: a timed-out classifier must never turn into a guess that loosens a
constraint.

## Decision

- `routed_classify::Classifier` is a synchronous, CPU-bound trait:
  `classify(&ClassifyInput) -> Result<Findings, ClassifyError>`. The router
  runs it on a bounded blocking pool under a strict per-classifier timeout
  (`RouterProfile.spec.classifier.timeoutMs`); the engine itself never waits.
- Implementations are selected by `RouterProfile.spec.classifier.type`:
  `heuristic` (default, always available), `stub` (tests, goldens),
  `http` (phase 2), `onnx` (phase 4, cargo feature `onnx`). Unknown or
  unavailable types fail at profile load, not per request.
- Input is the last user message, a truncated system prompt and every tool
  output (tool outputs are the main injection vector).
- A classifier error or timeout produces `Findings::degraded = [name]` and
  nothing else. The engine treats any degraded finding as "classification
  failed" and applies the policy `fallbackDecision`, still subject to the data
  class (ADR-0003). No classifier ever reports a conservative *value*; the
  conservative behaviour lives in the engine where it is testable.
- Every implementation must pass `routed_classify::conformance::assert_conformant`.

## Consequences

- Phases 1 to 3 run entirely on the heuristic classifier.
- A missing risk score is never permissive: when the policy sets
  `denyIfRiskScoreAbove` or any candidate tier caps `maxRiskScore` below 1, the
  engine records `risk:missing` as a degraded classifier and takes the fallback
  path. A risk score that *is* present is always enforced (policy block and
  tier caps), even when other classifiers degraded.
- The fallback tier is never exempt from the data class, request facts (tools,
  context window, required capabilities) or a known risk score; when
  classification succeeded and simply nothing survived, every hard constraint
  applies to it.

## Alternatives considered

- Async trait: rejected; inference is CPU-bound and the blocking pool gives
  the same cancellation semantics with less ceremony.
- Returning "worst case" findings on timeout: rejected; it hides failures in
  telemetry and couples classifier and policy semantics.
