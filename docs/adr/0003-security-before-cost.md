# ADR-0003: Security constraints are evaluated before cost optimization

## Status

Accepted

## Context

routeD optimises primarily for cost and secondarily for quality and latency,
but it also enforces data-class, sovereignty, tenant, and block rules. If those
were expressed as weights, a sufficiently cheap tier could outweigh a
sovereignty requirement. EU AI Act Article 12 style logging and GDPR Article 30
records also require that the reason a model was (not) eligible is recorded.

## Decision

The decision pipeline has a fixed, non-configurable total order:

1. Extract request context and resolve the data class as the **maximum** of the
   explicit header class and the inferred class.
2. Apply hard constraints in order, each recording every eliminated tier with a
   reason: `denyIfRiskScoreAbove` (BLOCK), DataClass constraints (jurisdiction,
   CLOUD Act exposure, operator control, allowed data classes), capabilities
   and context window, `tier.security.maxRiskScore`, `maxCostPerRequest`.
3. If no candidate survives, apply the policy `fallbackDecision` **only if it
   satisfies the DataClass constraints**; otherwise BLOCK. Never "fall back to
   the cheapest".
4. Only then score the survivors (quality floor, weighted cost/quality/latency,
   optional learned router) and select.

Hard constraints are never weights. No environment variable, flag, or header can
bypass step 2; only a RoutingPolicy or DataClass change (versioned, auditable)
changes the outcome. Untrusted headers can only tighten (ADR-0007). The
Decision records the pre- and post-filter candidate sets, so the ordering is
provable from the audit record.

## Consequences

- A property test (Phase 1) asserts that for any random policy/tier set the
  selected tier satisfies every hard constraint and that eliminated tiers are
  never selected.
- Classifier failure is fail-safe: timeouts yield conservative findings and the
  policy's `fallbackDecision`, never a guess that could loosen constraints.
- Cost savings are reported relative to the most expensive *surviving*
  candidate, never to tiers that were eliminated for security reasons.

## Alternatives considered

- **Single weighted objective including security terms.** Rejected: allows
  trades between security and savings.
- **Configurable constraint order.** Rejected: makes audits depend on
  configuration and invites misconfiguration.
