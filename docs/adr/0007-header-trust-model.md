# ADR-0007: Header trust model

## Status

Accepted

## Context

Callers send `X-Routed-Tenant`, `X-Routed-Agent`, `X-Routed-Data-Class`,
`X-Routed-Policy` and `X-Routed-Dry-Run`. Clients are untrusted: a header must
never lower a data class, unlock a tier or select a policy the caller is not
entitled to. Decision headers (`X-Routed-Decision`, `X-Routed-Tier`, ...) are
emitted by routeD and must not be spoofable.

## Decision

- Ingress layers strip **every** inbound `x-routed-*` header before forwarding
  (`routed_security::is_routed_header`).
- `routed_security::extract_headers` parses the known inbound names into
  `routed_decision::RequestHints`, a type that can only express restriction:
  `data_classes` (all values, merged by maximum rank), `policy` (honoured only
  if it names a policy that already matches the request scope **and** that
  policy has `spec.overridable: true`, or is the winner anyway), `dry_run`.
  Without the explicit opt-in a caller could move a request from a strict
  high-priority policy to a permissive wildcard one.
  Unknown `X-Routed-*` names, malformed values (control characters, > 256
  bytes) and duplicates of single-valued headers are ignored and reported.
- The engine folds hints at exactly one point: the effective data class is the
  maximum rank over header values and inferred classes (PII inference honours
  each class's `minConfidence`); a policy override outside the matching set or
  to a non-overridable policy is ignored with a note.
- Tests: header downgrade attempts (unit and golden), spoofed decision
  headers, duplicates with conflicting values, and a property test asserting
  that for any random world the hinted candidate set is a subset of the
  unhinted one and the effective rank never decreases.

## Consequences

- A header can make routing stricter or select among allowed policies; it can
  never make it looser. Downgrade attempts are visible in `Decision.notes`.

## Alternatives considered

- Trusting headers from mTLS-authenticated gateways: deferred; the default
  stays untrusted, and any future authenticated-caller support must not
  loosen it.
