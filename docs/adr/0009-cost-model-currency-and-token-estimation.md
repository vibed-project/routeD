# ADR-0009: Cost model, currency and token estimation

## Status

Accepted

## Context

Cost caps and savings must be comparable across tiers priced in different
currencies, and must be estimated before the response exists.

## Decision

- Money is integer micro-EUR everywhere inside routeD (`MicroEur = u64`).
  `ModelTier.spec.cost` declares a currency (`EUR` default, `USD`); the
  compiler converts using `RouterProfile.spec.costModel.fxToEUR`. No rate for a
  non-EUR tier is a compile error; conflicting rates across profiles are an
  error. No live FX in core.
- `estimatedCost = ceil((inputTokens * inputPerMillion + outputTokens * outputPerMillion) / 1e6)`.
- Input tokens: `ceil(utf8_bytes / 4)` per message plus 4 per message (the
  `TokenEstimator` seam allows model-family tables later). Output tokens:
  `max_tokens` / `max_completion_tokens` / `max_output_tokens` from the
  request, else `RouterProfile.spec.costModel.defaultOutputTokens` (256).
- Decision JSON reports `estimatedCostEUR` (8 decimals) and
  `estimatedSavingsEUR` = cost of the most expensive *scored* candidate minus
  the selected cost. Tiers eliminated by hard constraints never count as
  savings.

## Consequences

- The spec field names (`maxCostPerRequestEUR`, `estimatedCostEUR`) stay as
  written; other currencies are inputs, EUR is the reporting currency.
- Estimates are estimates: the feedback API (phase 6) carries actual usage.

## Alternatives considered

- Floating-point EUR: rejected (non-deterministic rounding across platforms).
- tiktoken-style tokenisers for caps: rejected for v0.1 (weight and licensing);
  the seam exists.
