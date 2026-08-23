# trainer

Python 3.12 (`uv`) project for offline learned-router training (ADR-0018).
Never runs in the request path.

## Workflow

1. Run the router with `--feedback-dir /var/lib/routed/feedback` (Helm:
   `feedback.enabled=true`). It appends `decisions.jsonl` (the decision
   journal: labels and routing facts, never request content) and
   `feedback.jsonl` (accepted `POST /v1/feedback` bodies).
2. Get the compiled snapshot the decisions were made against:
   `routedctl validate my-resources.yaml --emit-snapshot snapshot.json`
   (or read the operator's fallback ConfigMap).
3. Train and export:

   ```sh
   uv run routed-train \
     --decisions feedback/decisions.jsonl \
     --feedback feedback/feedback.jsonl \
     --snapshot snapshot.json \
     --out out/
   ```

   `out/model.onnx` implements the ADR-0018 contract (`features` [1, N]
   in; `quality` and `confidence` out) over the `routed-features/1`
   layout - byte-for-byte the runtime's
   `crates/classify/src/router_features.rs`. `report.json` carries the
   validation accuracy (also baked in as the constant `confidence` head)
   and `calibration.json` the validation probability quantiles per quality
   floor.

4. Pin and deploy: hash the model (`sha256sum out/model.onnx`), publish it,
   and set `RouterProfile.spec.learnedRouter.uri` to
   `https://.../model.onnx@sha256:<hex>`. Enable per policy with
   `RoutingPolicy.spec.learnedRouter.enabled` and `minConfidence`. The
   router (built with the `onnx` feature) loads it at startup; predictions
   only refine `predictedQuality` - hard constraints are untouched.

## Fixtures

`scripts/make_classifier_fixture.py` and `scripts/make_router_fixture.py`
regenerate the committed test fixtures in
`crates/classify/tests/fixtures/`; they are fixed-weight instances of the
same graphs this trainer exports.
