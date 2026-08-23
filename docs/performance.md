# Performance

## Budgets

| Path | Budget | Gate |
|------|--------|------|
| Decision engine only (no classifiers), 50 tiers, 5 policies, data class active | p95 < 1 ms | `crates/decision/tests/latency_gate.rs` |
| Engine + local ONNX classifiers (phase 4) | p95 < 30 ms added latency | `crates/classify/tests/onnx.rs::perf_gate` via `ROUTED_PERF=1 make onnx` |

## Running the gates

```sh
ROUTED_PERF=1 make test                       # engine gate (skipped without ROUTED_PERF)
ROUTED_PERF=1 ROUTED_PERF_SLACK=1.5 make test  # noisy VMs: multiply the budget
ROUTED_PERF=1 make onnx                       # classifier gate (downloads libonnxruntime into .cache/)
scripts/cargo-in-podman.sh cargo bench -p routed-decision   # criterion trend numbers
scripts/cargo-in-podman.sh cargo bench -p routed-classify   # heuristic classifier trend numbers
```

The classifier gate runs against the committed fixture model
(`crates/classify/tests/fixtures/`, regenerable with
`trainer/scripts/make_classifier_fixture.py`), so it bounds the
tokenize -> run -> extract pipeline, not a production model. Re-calibrate
when a real trained classifier lands (phase 6).

## Reference runner class

Gates are calibrated for GitHub-hosted `ubuntu-latest` (x64, 4 vCPU). Numbers
from the local podman machine (arm64 VM, 6 vCPU) are informational only.

## Recorded results

| Date | Environment | p50 | p95 | p99 |
|------|-------------|-----|-----|-----|
| 2026-08-22 | podman machine arm64, 6 vCPU, debug build | see CI log | | |
| 2026-08-22 | classifier gate (fixture model), same VM: 200 runs in 0.43 s, p95 well under budget | | | |
