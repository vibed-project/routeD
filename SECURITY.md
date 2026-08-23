# Security Policy

## Reporting a Vulnerability

Please report vulnerabilities privately through GitHub Security Advisories for
`vibed-project/routed` ("Report a vulnerability"). Do not open public issues for
security problems. You will receive an acknowledgement within 3 business days.

## Supported Versions

routeD is pre-1.0. Only the latest `0.x` release receives security fixes.

## Security Model (summary)

- routeD is a **decision layer**, not a gateway. It never holds provider API
  keys, never calls model providers, and never terminates client TLS for the
  data plane on its own behalf. The gateway in front of the providers enforces
  every decision.
- Hard constraints (data class, sovereignty, tenant policy, block rules) are
  evaluated **before** any cost or quality optimization and prune the candidate
  set. No configuration flag can reorder this.
- **Untrusted request headers can only restrict.** `X-Routed-*` headers from
  callers may raise the data class or narrow the candidate set; they can never
  lower a data class or unlock a tier. Inbound `X-Routed-*` headers are
  stripped before forwarding.
- **Fail safe, not fail open.** Classifier timeouts apply the policy's
  `fallbackDecision`; a router with no snapshot is not ready; Envoy integration
  is documented with `failure_mode_allow: false`.
- **No prompt persistence.** Prompts are never logged. An optional salted hash
  can be enabled for correlation.
- Request bodies are size-limited before parsing. Model artifacts are
  digest-pinned (https and oci pulls verify the full chain, ADR-0016/0019);
  cosign signature verification for artifacts is a tracked gap in the threat
  model.

The full threat model lives in [`docs/threat-model.md`](docs/threat-model.md).
