# ADR-0011: Golden decisions live in examples/

## Status

Accepted

## Context

The spec requires golden tests over `examples/` request + policy combinations
whose stored `Decision` JSON must not drift.

## Decision

- Each `examples/NNN-<outcome>-<scenario>/` directory holds `resources.yaml`,
  `request.json`, optional `headers.json`, `findings.json`, `overrides.json`,
  `path.txt`, and the golden `expected.decision.json`.
- `cmd/routedctl/tests/golden.rs` runs every directory through the same code
  path as `routedctl explain --dir` with a fixed decision id and compares the
  pretty JSON byte for byte. `UPDATE_GOLDEN=1` rewrites expectations; the diff
  is reviewed like a behaviour change.
- Determinism by construction rather than scrubbing: the decision id is
  injected, latency is not measured offline, floats are rounded at
  serialisation (6 decimals for scores, micro-EUR for money), the snapshot hash
  is real and therefore guards compiler determinism too.
- The golden test also asserts that every `EliminationReason` variant appears
  in at least one example.

## Consequences

- Examples are documentation, CLI demo material and CI fixtures at once.

## Alternatives considered

- `insta` snapshots: rejected; a second snapshot format next to the
  user-facing examples layout adds tooling without adding coverage.
