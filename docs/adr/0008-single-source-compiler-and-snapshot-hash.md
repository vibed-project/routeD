# ADR-0008: Single-source policy compiler and canonical snapshot hash

## Status

Accepted

## Context

The operator, the admission webhook, `routedctl validate/explain` and the test
suites all need to turn CRDs into something the engine can evaluate. Three
implementations would drift. Audit records need to name the exact
configuration a decision was made against.

## Decision

- `routed_policy::compile(&CompileInput) -> Result<(Snapshot, CompileReport), CompileError>`
  is the only compiler. Diagnostics (`Diag { level, kind, name, field, message }`)
  print identically in every consumer.
- The compiler is pure and deterministic: inputs are keyed and sorted
  (`BTreeMap`, sorted candidate lists, policies ordered by priority desc then
  `namespace/name`), so input order never changes the output.
- `Snapshot.hash = sha256(canonical JSON of SnapshotCore)`. The core excludes
  all Kubernetes metadata except names and namespaces; `compilerVersion` and
  `schemaVersion` are part of the hashed content. Routers verify the hash on
  receipt; the hash appears in every `Decision` and on every span.
- Prices are converted to micro-EUR integers at compile time using the
  `RouterProfile.spec.costModel.fxToEUR` table; a missing rate is a compile
  error (ADR-0009).
- Validation rules (errors): names unique within a snapshot, references
  resolve (tiers, data classes, profiles), numeric ranges, candidate sets not
  empty. Warnings: shadowed policies (same priority and match), fallback tiers
  that cannot serve a data class, tiers claiming a data class whose
  constraints they violate, selectors matching nothing, policies with no
  routed aliases.

## Consequences

- `routedctl explain` reproduces the operator's snapshot byte for byte.
- Tier, data class and profile names must be unique across namespaces within
  one snapshot; the operator scopes snapshots accordingly (ADR-0014).

## Alternatives considered

- Distributing raw CRDs to routers: rejected; every router would re-validate
  and version skew would be invisible.
