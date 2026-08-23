# routed Helm chart

Installs the routeD semantic router (and optionally the operator).

```sh
helm install routed ./charts/routed --set mode=inline --set upstream=http://litellm:4000
```

| Value | Default | Description |
|-------|---------|-------------|
| `mode` | `inline` | `inline` forwarder or `extproc` Envoy external processor |
| `upstream` | `""` | Gateway URL for inline mode |
| `image.repository` / `image.tag` | `ghcr.io/vibed-project/routed` / appVersion | Router image |
| `replicaCount` | `1` | Router replicas |
| `service.port` / `service.extprocPort` | `8080` / `9002` | HTTP and ext_proc gRPC ports |
| `routing.enabled` / `routing.files` / `routing.existingConfigMap` | `false` | Routing resources mounted as a ConfigMap (snapshot source until the operator ships) |
| `otlpEndpoint` | `""` | OTLP gRPC endpoint for trace export |
| `operator.enabled` | `false` | Deploy the operator |
| `operator.leaderElect` | `true` | Leader election for the operator |
| `operator.watchNamespace` | `""` | Restrict the operator to one namespace |
| `podSecurityContext` / `securityContext` | hardened | Non-root, read-only FS, no capabilities |

The chart uses `apiVersion: v2` and only Helm 3 compatible constructs; it is
linted with Helm 4 in CI. CRDs will ship under `crds/` once generated.
