---
sidebar_position: 3
title: How routing works
---

# How routing works

This page describes what happens between a request arriving and a model being
chosen, in enough detail to decide whether the behaviour is trustworthy. The
short version: classification informs, constraints decide, scoring only ever
picks among candidates that already satisfy every constraint, and the whole
thing is written down in one auditable document.

The engine itself (`crates/decision`) is pure and deterministic. It performs no
I/O, so the same snapshot plus the same request plus the same classifier
findings always produce the same decision, on any machine.

## The fixed order

| Step | What happens | Can it be reordered? |
|------|--------------|----------------------|
| 1 | Extract request context: tenant, agent, path, requested model, tools, token estimate, and `X-Routed-*` hints | No |
| 2 | Select exactly one `RoutingPolicy` by priority, then name; if none matches the request scope, return `PASS_THROUGH` | No |
| 3 | If the requested model is not one of the winning policy's `modelAliases`, return `PASS_THROUGH` | No |
| 4 | Run classifiers in parallel under per-classifier timeouts | No |
| 5 | Resolve the effective data class as the maximum of the header class and the inferred class | No |
| 6 | Build the candidate set from `spec.candidates` | No |
| 7 | Apply hard constraints in a fixed order, recording every elimination | No |
| 8 | If nothing survives, apply `fallbackDecision` only if it satisfies the data class, else `BLOCK` | No |
| 9 | Score the survivors and select | Weights are configurable, the position of this step is not |

No environment variable, command-line flag or request header can move step 9
ahead of step 7, and no header can weaken step 7. Changing an outcome requires
changing a `RoutingPolicy` or a `DataClass`, which is a versioned, auditable
resource edit. The reasoning is in
[ADR-0003](./adr/0003-security-before-cost.md).

## Step 2: which policy applies

Several policies can match one request. They are evaluated in a total order,
`spec.priority` descending and then `namespace/name` ascending, and the first
policy whose scope matches wins. Policies are never merged, so an explanation
names exactly one policy and "why this policy" is answered by ordering alone.

Scope matching uses simple globs: `*` matches everything, `prefix*` and
`*suffix` are supported, anything else is exact and case-sensitive, and an
empty list matches everything. The compiler warns when two policies in one
namespace share a priority and an identical match. Details in
[ADR-0010](./adr/0010-policy-precedence.md).

A caller can send `X-Routed-Policy`, but it is honoured only for a policy that
already matches the request scope and that sets `spec.overridable: true`.
Otherwise the attempt is ignored and recorded in the decision's `notes`, so an
untrusted header can never move a request to a looser policy.

## Step 4: classification

Classification is local. Nothing leaves the pod unless you configure an
external classifier yourself.

### What is classified

| Finding | Values | Used for |
|---------|--------|----------|
| Task | A label from `RouterProfile.spec.classifier.labels.task`; the heuristic classifier emits `code`, `summarization`, `translation`, `extraction`, `reasoning` or `chat` | Per-task quality priors (`ModelTier.spec.quality.byTask`) |
| Complexity | `low`, `medium`, `high` | Reasoning budget, and the learned router's feature vector |
| Sensitivity and PII | Entity types such as `EMAIL`, `IBAN`, `PHONE`, `NATIONAL_ID`, `HEALTH`, `CREDIT_CARD`, with confidences | Inferring the data class, which drives hard constraints |
| Injection risk | A score in `0..=1` | `denyIfRiskScoreAbove` and per-tier `security.maxRiskScore` |

The classified text is the same for every implementation: the last user
message, a truncated system prompt, and every tool output. Tool outputs are
included because they are the main prompt-injection vector. Earlier user turns
are additionally scanned for PII.

### Implementations

`RouterProfile.spec.classifier.type` selects the implementation. Every one of
them must pass the same in-tree conformance suite.

| Type | Availability | Notes |
|------|--------------|-------|
| `heuristic` | Always, and the default | Regex task rules, length-based complexity, regex plus checksum PII detection (IBAN mod-97, Luhn for cards), and twelve weighted prompt-injection patterns |
| `onnx` | Behind the `onnx` cargo feature | One multi-head encoder for task, complexity, sensitivity and risk |
| `http` | Always | Delegates to your own service over JSON |
| `stub` | Tests and goldens | Fixed findings |

The PII detectors return entity types, confidences and byte spans only, never
the matched text, so findings are safe to log.

The ONNX classifier does not replace the heuristics, it composes with them.
PII spans always come from the heuristic detectors, and the reported risk score
is `max(model risk, heuristic injection score)`. A model can therefore only
tighten what the heuristics already caught, never loosen it. Artifacts are
digest-pinned and verified before use, and a profile whose model fails to load
stops startup rather than degrading silently. The tensor contract and the
artifact rules are in
[ADR-0016](./adr/0016-artifact-resolution-and-onnx-contract.md).

For `type: http`, the request and response shapes are documented in the
[external classifier contract](./classifier-http.md).

### The effective data class

Two things can set a data class: the `X-Routed-Data-Class` header (matched
against each `DataClass.spec.detection.headerValues`) and inference from
detected PII entities (subject to `detection.minConfidence`). The effective
class is the one with the **highest** `spec.rank` among them. A header can
therefore raise the class but never lower it. Downgrade attempts appear in the
decision's `notes`. See [ADR-0007](./adr/0007-header-trust-model.md).

## Step 7: hard constraints, in order

Each stage eliminates candidates and records why. Elimination reasons come from
a closed vocabulary, listed in full in the [decision API](./decision-api.md).

| Order | Constraint | Effect |
|-------|-----------|--------|
| 1 | `hardConstraints.denyIfRiskScoreAbove` | Blocks the request outright, nothing is forwarded |
| 2 | `DataClass` constraints: `requireJurisdiction`, `forbidCloudActExposed`, `requireOperatorControl`, `allowedDataClasses`, `forbidCapabilities`. Applied when `hardConstraints.respectDataClass` is set, which it is by default | Eliminates tiers |
| 3 | Capabilities required by the policy and by the request, and context window | Eliminates tiers |
| 4 | `ModelTier.spec.security.maxRiskScore` and `toolCallingAllowed` | Eliminates tiers |
| 5 | `hardConstraints.maxCostPerRequestEUR` | Eliminates tiers |

### Why security comes before cost

If sovereignty were a weight rather than a filter, a sufficiently cheap tier
could outweigh a jurisdiction requirement. The order above makes that
impossible by construction, and makes the ordering provable from the audit
record: the decision carries both the pre-filter candidate set and the
post-filter one, each elimination attributed to a named constraint.

Three consequences follow, and each is enforced in code rather than by
convention:

- **The fallback is not an escape hatch.** When no candidate survives,
  `fallbackDecision.tier` is used only if it satisfies the data class and the
  request facts. Otherwise the outcome is `BLOCK`. There is no "fall back to
  the cheapest".
- **Savings are honest.** `estimatedSavingsEUR` is computed against the most
  expensive *surviving* candidate. A tier eliminated for sovereignty reasons
  never inflates the savings number.
- **A property test enforces it.** For randomly generated policy and tier sets,
  the selected tier satisfies every hard constraint and eliminated tiers are
  never selected.

### What this ordering does not protect against

Two honest limits, both recorded in the [threat model](./threat-model.md):

- **Task labels are not a security boundary.** A caller who can freely rephrase
  a prompt can steer the *task* classification, for example by phrasing
  everything as chat. Write policies so that data classes and hard constraints,
  not task labels, carry the security decisions.
- **The gateway is inside the trust base.** routeD rewrites the request; the
  gateway enforces it. A gateway that ignores the rewritten `model` defeats
  routing entirely, which is why every integration guide requires honouring it
  and failing closed.

## Step 9: scoring the survivors

Only now does optimisation happen.

**Quality floor.** If `objective.qualityFloor` is set, candidates whose
predicted quality falls below it are eliminated with reason `qualityFloor`. If
that would empty the set, the best available quality is kept instead and the
decision records why.

**Weights.** `objective.mode` sets the default weights; `objective.weights`
overrides them and is normalised to sum to 1.

| `objective.mode` | cost | quality | latency |
|------------------|-----:|--------:|--------:|
| `cost-first-with-quality-floor` | 0.6 | 0.3 | 0.1 |
| `quality-first` | 0.1 | 0.8 | 0.1 |
| `balanced` | 0.333 | 0.333 | 0.333 |
| `latency-first` | 0.1 | 0.2 | 0.7 |

**Score.** Cost, quality and p50 latency are min-max normalised across the
surviving set, then combined so that cheaper and faster score higher:

```text
score = w_cost * (1 - cost_norm) + w_quality * quality_norm + w_latency * (1 - latency_norm)
```

Ties are broken deterministically: score descending, then cost ascending, then
tier name ascending. Scores are rounded to six decimals so explanations are
stable across architectures.

**Where the numbers come from.** Quality is `ModelTier.spec.quality.byTask` for
the classified task, falling back to `quality.baseline`. Cost is estimated
before the response exists: input tokens are approximated from the request,
output tokens come from `max_tokens` (or its equivalents) or from
`RouterProfile.spec.costModel.defaultOutputTokens`. Money is integer micro-EUR
internally, and non-EUR tier prices are converted at compile time using
`costModel.fxToEUR`; a non-EUR tier with no rate is a compile error. There is
no live FX lookup. See
[ADR-0009](./adr/0009-cost-model-currency-and-token-estimation.md).

**The learned router.** When `RoutingPolicy.spec.learnedRouter.enabled` is set
and the prediction's confidence clears `minConfidence`, a trained model refines
`predictedQuality`. That is the whole of its influence: it runs after hard
constraints, it cannot resurrect an eliminated tier, and without the model the
engine falls back to the tier priors. Contract in
[ADR-0018](./adr/0018-feedback-records-and-learned-router.md).

## The explanation

Every decision serialises to one JSON document, identical in `routedctl
explain`, the `X-Routed-Decision` response header (base64, size-capped) and the
`routed.decision` OpenTelemetry span. It records the matched policy, the
requested model and the selected tier and gateway model, the effective data
class, the classification, every candidate with either a score or an
`eliminatedBy` reason, the estimated cost and savings, the decision latency,
and the `snapshotHash` the decision was made against.

`routedctl explain` renders the same data as a trace:

```text
ROUTE  policy=ai-platform/default-cost-secure  model=auto -> mistral-large-eu
  data class: personal   task: chat   complexity: low   risk: 0.000
  tokens in/out: 15/256   tenant: -   hints: RequestHints { data_classes: ["personal"], policy: None, dry_run: false }
  * eu-large             selected  quality=0.820 cost=EUR 0.001566 score=0.7000
  x eu-small             eliminated by qualityFloor
  x us-cheap             eliminated by dataClass.allowedDataClasses
  estimated cost EUR 0.001566, savings EUR 0.000000 vs most expensive surviving candidate
  snapshot sha256:6082576...
```

Prompts never appear in any of these. The optional `--log-prompt-hashes` flag
records a salted hash of the classified text, and nothing else. Field
reference: [decision API](./decision-api.md); telemetry schema:
[ADR-0013](./adr/0013-telemetry-schema.md).

Because the snapshot hash is part of every decision, an audit can tie an
outcome to an exact configuration, and `routedctl explain` can reproduce that
outcome offline from the same resources.

## When classifiers are unavailable

Classifiers fail: models time out, HTTP services go down. routeD's rule is that
a failure is never converted into a guess.

A classifier error or timeout produces exactly one thing: a `degraded` marker
naming the classifier, and no findings at all. No implementation may return
"worst case" values; the conservative behaviour lives in the engine, where it
is testable. The engine treats any degraded finding as "classification failed"
and applies the policy's `fallbackDecision`, still subject to the data class
and to the request facts. See
[ADR-0006](./adr/0006-classifier-seam-and-degradation.md).

A degraded decision says so:

```json
{
  "outcome": "ROUTE",
  "selectedTier": "eu-sovereign-large",
  "classification": {},
  "reason": "classification degraded; using fallbackDecision.tier",
  "fallback": true,
  "degraded": ["heuristic:timeout", "risk:missing"]
}
```

`risk:missing` is recorded when no risk score was available but one was
required, which is the case whenever the policy sets `denyIfRiskScoreAbove` or
any candidate tier caps `maxRiskScore` below 1. A missing risk score is never
treated as a low risk score.

Partial degradation still enforces what is known. If the PII detector times out
but a risk score was produced, that score is enforced, block rules included:

```json
{
  "outcome": "BLOCK",
  "classification": { "riskScore": 0.99 },
  "reason": "risk score 0.99 exceeds denyIfRiskScoreAbove 0.95",
  "degraded": ["pii:timeout"]
}
```

Other failure behaviours in the same spirit:

- A router with no snapshot is **not ready**, so it receives no traffic.
- A configured model artifact that fails to load or fails digest verification
  stops startup rather than serving with a silently missing classifier.
- Unparseable or oversized request bodies fail closed: nothing is forwarded.
- Gateways are told to fail closed too (`failure_mode_allow: false`), so
  routeD being unreachable does not silently disable routing.

## How this is verified

| Property | How |
|----------|-----|
| Constraints always precede scoring | Property tests over randomly generated policy and tier sets |
| Explanations are stable | The `examples/` directory doubles as golden decision fixtures, compared byte for byte |
| Headers can only restrict | Golden cases for downgrade attempts, spoofed decision headers and conflicting duplicates, plus a property test that hinted candidate sets are a subset of unhinted ones |
| Compiler agreement | `routedctl validate`, the admission webhook and the operator run the same compiler, and the snapshot hash proves it |
| Latency | Dedicated gates hold the engine alone to p95 under 1 ms and local ONNX classification to p95 under 30 ms of added latency; run them with `ROUTED_PERF=1 make test` and `ROUTED_PERF=1 make onnx` (see [performance](./performance.md)) |

## Related reading

- [Architecture](./architecture.md): where each of these steps runs.
- [Threat model](./threat-model.md): what an attacker can and cannot do to a
  decision.
- [ADR index](./adr/README.md): every contract referenced above.
