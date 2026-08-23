# End-to-end tests

`make e2e` creates a kind cluster `routed-e2e` (podman provider, no host port
mappings), builds and loads the `routed` and `routed-mockgateway` images, installs
the Helm chart in inline mode with `examples/001` resources mounted as a
ConfigMap, and runs the assertions in `run.sh` through `kubectl port-forward`:
route with rewritten model, pass-through, BLOCK on injection, EU-only selection
with `X-Routed-Data-Class: personal`, dry-run, SSE streaming integrity, the
decision API and metrics.

`E2E_KEEP=1 make e2e` keeps the cluster for debugging; `make e2e-down` deletes it.
The operator, Envoy ext_proc and OTel collector scenarios join in phases 3 and 5.
