# Decision API

> Field semantics are implemented in `crates/decision` (phase 1); the HTTP wire format lands in phase 2.

## `POST /v1/decide`

Body: the raw OpenAI-format request (same as would be sent to the gateway).
Headers: the same `X-Routed-*` request headers. Idempotent; no side effects
beyond telemetry. Response: the `Decision` JSON below.

## `POST /v1/feedback`

```json
{ "decisionId": "01J...", "outcome": { "rating": 4, "success": true, "tokensIn": 812, "tokensOut": 133, "latencyMs": 1430, "costEUR": 0.0031 }, "source": "agent" }
```

Accepted feedback is persisted (with the decision journal) when the router
runs with `--feedback-dir`, and consumed by the offline trainer; the
trainer labels a decision positive on `success: true` or `rating >= 4`
(ADR-0018). Feedback never changes routing online.

## Request headers (untrusted; may only restrict)

| Header | Effect |
|--------|--------|
| `X-Routed-Tenant` | Tenant identity for policy matching |
| `X-Routed-Agent` | Agent identity for policy matching |
| `X-Routed-Data-Class` | Explicit data class; merged with inferred class, most restrictive wins |
| `X-Routed-Policy` | Policy override; honoured only for a policy that matches the request scope and has `spec.overridable: true` |
| `X-Routed-Dry-Run: true` | Return the decision without rewriting |

All inbound `X-Routed-*` headers are stripped before the request is forwarded.

## Response headers

`X-Routed-Decision-Id`, `X-Routed-Tier`, `X-Routed-Data-Class`,
`X-Routed-Outcome`, `X-Routed-Decision` (base64 JSON, size-capped; the full
document is on the span), `X-Routed-Estimated-Cost`.

## `Decision`

| Field | Type | Notes |
|-------|------|-------|
| `id` | string (ULID) | Decision id, correlates feedback |
| `outcome` | `ROUTE` / `PASS_THROUGH` / `BLOCK` | |
| `policy` | `namespace/name` | Matched RoutingPolicy |
| `requestedModel` | string | Model alias the caller asked for |
| `selectedTier`, `gatewayModel` | string | Present for `ROUTE` |
| `parameters` | object | Reasoning budget, token caps to inject |
| `dataClass` | string | Effective data class |
| `classification` | object | `task`, `complexity`, `riskScore`, `piiEntities` |
| `candidates[]` | array | Every candidate with `eliminatedBy` or scores; `selected: true` on the winner |
| `estimatedCost*`, `estimatedSavings*` | number | Savings vs the most expensive surviving candidate; currency suffix per policy |
| `latencyMs` | number | Decision latency |
| `snapshotHash` | `sha256:...` | Snapshot the decision was made against |
| `reason` | string | Why the request was blocked, passed through, or routed via fallback |
| `fallback` | bool | `true` when `fallbackDecision.tier` was applied (omitted when false) |
| `degraded` | []string | Classifiers that failed or timed out (`risk:missing` when no risk score was available but required) |
| `notes` | []string | Ignored hints and headers (for example a downgrade attempt or a non-overridable policy override) |
| `dryRun` | bool | `true` when `X-Routed-Dry-Run` was set (omitted when false) |

Every `eliminatedBy` value comes from a closed set: `hardConstraints.denyIfRiskScoreAbove`,
`dataClass.allowedDataClasses`, `dataClass.requireJurisdiction`, `dataClass.forbidCloudActExposed`,
`dataClass.requireOperatorControl`, `dataClass.forbidCapabilities`, `hardConstraints.requireCapabilities`,
`capabilities.tools`, `capabilities.contextWindow`, `security.maxRiskScore`, `security.toolCallingAllowed`,
`hardConstraints.maxCostPerRequestEUR`, `qualityFloor`.

See `README.md` for a full example.
