# ADR-0010: Policy precedence

## Status

Accepted

## Context

Several `RoutingPolicy` objects may match one request (tenant, agent, path).

## Decision

- Policies are evaluated in a total order: `spec.priority` descending, then
  `namespace/name` ascending. The first policy whose scope matches wins;
  policies are never merged.
- Scope matching uses simple globs: `*` matches everything, `prefix*` and
  `*suffix` are supported, everything else is exact and case-sensitive. An
  empty list matches everything.
- Whether the request is routed at all is decided by the winning policy's
  `match.modelAliases`; a non-matching requested model is `PASS_THROUGH`.
- The compiler warns when two policies in one namespace share priority and an
  identical match (the lexicographically smaller name always wins).
- `X-Routed-Policy` can select a policy only among those whose scope matches
  the request (ADR-0007).

## Consequences

- Explanations name exactly one policy; "why this policy" is answered by
  ordering alone.

## Alternatives considered

- Most-specific-match wins: rejected; specificity is ambiguous across tenant,
  agent and path dimensions, priority is explicit.
