# ADR-0018: Feedback records and the learned router contract

## Status

Accepted

## Context

Phase 6 closes the loop: outcomes reported to `POST /v1/feedback` must reach
offline training, and a trained model must be able to sharpen the engine's
quality estimates at decision time. The seams already exist: the pure engine
has a `QualityPredictor` hook (feeding `predictedQuality`, still subject to
every hard constraint), `RouterProfile.spec.learnedRouter` carries the
artifact reference and calibration, and `RoutingPolicy.spec.learnedRouter`
gates it per policy with `minConfidence`. What is missing is the data
contract between router, exporter and trainer - and it must honour the
invariant that prompts are never logged or persisted.

## Decision

### Two JSONL streams, joined on `decisionId`

When `--feedback-dir` (`ROUTED_FEEDBACK_DIR`) is set, the router appends two
append-only JSONL files via a bounded channel and a writer task (the hot
path never blocks on disk; on a full channel records are dropped and
counted, never awaited):

- `decisions.jsonl`: one record per decision - id, UTC timestamp, mode,
  outcome, policy, selected tier and gateway model, data class, the
  classifier findings **minus any text** (task, complexity, risk score, PII
  entity names only), estimated tokens and cost, and the snapshot hash. No
  prompt-derived strings beyond the closed label vocabularies.
- `feedback.jsonl`: one record per accepted `POST /v1/feedback` - decision
  id, timestamp, source, and the caller's `outcome` object (bounded, JSON
  as sent).

The trainer joins the two on `decisionId`. `crates/feedback` owns the
record types and the `FeedbackSink` seam (ADR-0004); the JSONL sink is the
in-tree implementation; log-shipping and warehouse exporters plug the same trait.

### Learned router feature vector (versioned: `routed-features/1`)

The runtime and the trainer must agree byte-for-byte on features. Version 1
is deliberately small, deterministic and embedding-free:

```
[ task one-hot          x len(profile.classifier.labels.task)   (unknown task = all zeros)
, complexity one-hot    x 3                                     (low, medium, high)
, risk score            x 1                                     (0 when absent)
, tier quality prior    x 1                                     (tier.quality_for(task))
, tier log10 cost       x 1                                     (log10(1 + in+out micro-EUR per MTok))
, tier latency p50      x 1                                     (ms / 1000)
]
```

Tiers are described by features, not identity, so a model survives tier
renames and additions (degrading gracefully rather than breaking).

### Learned router ONNX contract (feature `onnx`)

- Input `features`: f32 `[1, N]` with N as above.
- Output `quality`: f32 `[1]` or `[1, 1]`, a probability in `[0, 1]` that
  the tier meets the quality bar for this request. Required.
- Output `confidence`: f32 `[1]`, optional; defaults to 1.0 when absent.
  Compared against `RoutingPolicy.spec.learnedRouter.minConfidence` by the
  engine.

The predictor loads `learnedRouter.uri` through `routed-artifact`
(digest-pinned, ADR-0016) and implements the engine's `QualityPredictor`;
a prediction can only refine `predictedQuality` - hard constraints,
data classes and the fallback ladder are untouched. Without the `onnx`
feature or without a configured artifact the engine keeps using tier
priors, exactly as today.

### Trainer (`trainer/`)

`uv run routed-train` consumes the two JSONL files, joins them, featurises
with the same layout, fits a logistic model (numpy only), and exports the
ONNX graph above plus a calibration report. The committed test fixture
(`make_router_fixture.py`) is a fixed-weight instance of the same graph so
the runtime contract is testable without training.

### `routedctl simulate`

Replays a JSONL file of requests (one OpenAI-format request per line, or
`{"request": ..., "path": ..., "headers": {...}}`) against a snapshot with
the same offline pipeline as `routedctl explain`, and prints aggregate
outcomes: counts per outcome and tier, total estimated cost and savings,
block reasons, and data class distribution. What-if analysis before a
policy change ships; no cluster required.

## Consequences

- Feedback persistence is opt-in and local; log shipping is the operator's
  choice, and losing feedback records only ever degrades future training,
  never routing.
- A trained model's features are reproducible from the decision journal
  alone; no request content is needed or stored.
- Changing the feature layout is a breaking change and bumps the
  `routed-features` version in this ADR.

## Alternatives considered

- Embedding-based features (via `crates/embed`): deferred; they need the
  embedder artifact pipeline at decision time and complicate the privacy
  story. The layout version leaves room.
- Online learning in the router: rejected (feedback never changes routing
  online); determinism and auditability win.
- Storing decision records inside the feedback POST body contract:
  rejected; callers should not need to echo router internals back.
