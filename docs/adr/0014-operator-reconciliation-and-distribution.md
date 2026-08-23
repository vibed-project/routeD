# ADR-0014: Operator reconciliation and snapshot distribution

## Status

Accepted

## Context

Phase 1 gave every kind a `status` subresource shaped for this
(`CommonStatus` / `RoutingPolicyStatus` with `conditions` and, on
`RoutingPolicy`, `compiledHash`). Phase 2's router only reads a snapshot from
local files. Phase 3 adds `routed-operator`: it must watch the four CRDs,
compile them with the same `routed-policy` compiler the router and
`routedctl` use, distribute the result to routers, and report per-object
status. It must also validate CRDs before they land (a validating webhook).

## Decision

### One global snapshot, computed independently per replica

`RoutingPolicy.match` scopes requests by tenant/agent/path at decision time
(ADR-0010), not by namespace at compile time, so the operator lists all four
kinds cluster-wide (or within `--watch-namespace`) and compiles one snapshot,
exactly like `routedctl` and the router's file source do. `routed_policy::compile`
is pure and deterministic, so every operator replica that observes the same
object set produces byte-identical output including the hash. Distribution to
routers therefore needs no coordination: each replica watches, compiles and
serves its own gRPC watchers independently. This gives HA on the read path for
free and keeps the reconcile loop a simple "any of the four kinds changed ->
relist all four -> compile -> swap" with no leader-only fast path to get wrong.

### Primary distribution: gRPC `SnapshotService.Watch`

A new `crates/proto` crate holds one tonic service:

```proto
service SnapshotService {
  rpc Watch(WatchRequest) returns (stream SnapshotChunk);
}
message WatchRequest { string client = 1; }
message SnapshotChunk { string snapshot_json = 1; }
```

The payload is the canonical `routed_snapshot::Snapshot` JSON (the same shape
`routedctl explain` and the file source already produce), not a
field-by-field protobuf mirror of every CRD type. Decision: "one JSON shape
everywhere" (README/CLAUDE.md convention) extends to the snapshot; protobuf is
only the transport. The operator keeps a `tokio::sync::watch<Option<Arc<Snapshot>>>`
updated by the reconciler; each RPC subscribes a receiver and streams the
current value immediately, then every subsequent change, with no
per-connection state beyond that.

### Fallback distribution: ConfigMap-mounted file

The router's phase-2 file source expects raw resource YAML and compiles it
itself; that is the wrong shape for a fallback that must stay authoritative
with the primary path. Instead the operator, when it is the elected leader,
writes the same compiled `Snapshot` JSON into a `ConfigMap`. The Helm chart
mounts that `ConfigMap` into the router pod as a file; a new router-side
source (`--snapshot-path`, alongside the existing `--resources`) polls the
file's mtime and loads the `Snapshot` directly (`serde_json`, no
recompilation). Both distribution paths therefore always agree with each
other and with `routedctl explain` on the same objects, because there is
exactly one compile step, done once, in the operator.

Selection in `routed serve`: `--snapshot-addr` set -> gRPC source;
else `--snapshot-path` set -> file-based compiled-snapshot source; else
`--resources` set -> phase-2 local-compile file source (standalone /
no-operator use); else not ready.

### Leader election gates writes, not reads

Every replica reconciling and serving gRPC independently is safe because
compilation is pure, but every replica writing `status.conditions` on every
object, or writing the fallback `ConfigMap`, on every reconcile would cause
needless `resourceVersion` churn and API load proportional to replica count.
A `coordination.k8s.io/v1 Lease` (name `routed-operator-leader`, in the
operator's namespace) gates exactly those two write paths behind
`--leader-elect`; when disabled (default, single replica) the process always
acts as leader. Losing the lease stops writes; it does not stop compiling or
serving gRPC watchers, so in-flight routers keep receiving updates through a
leader handover.

### Status conditions from compile diagnostics

`CompileReport::diags` is already keyed by `(kind, "namespace/name")`
matching every object's own `(kind, metadata)`. After each compile, the
leader groups diagnostics by that key and, for every object of every kind,
writes a `Ready` condition (`True` with no matching error diagnostics,
`False` with the first error's message otherwise) and `observedGeneration`;
`RoutingPolicy` additionally gets `compiledHash` set to the new snapshot hash
when `Ready`.

### Validating webhook

Deferred to a fast-follow within phase 3: an admission webhook needs a
TLS-serving endpoint, a self-signed certificate bootstrapped into the
`ValidatingWebhookConfiguration`'s `caBundle`, and `failurePolicy` tuning that
is easy to get subtly wrong (e.g. locking out `kubectl apply` during a
webhook rollout). The reconciler, distribution and leader election above are
independently useful and independently testable; shipping them first and the
webhook second avoids a half-verified TLS bootstrap landing alongside
everything else. Until it lands, bad CRDs are caught by
`status.conditions[Ready]` (async, already implemented above) and
`routedctl validate` (existing, phase 1).

## Consequences

- Adding a replica improves gRPC read availability immediately; it never
  needs to "become ready" for writes because non-leader replicas simply don't
  attempt them.
- The router never talks to the Kubernetes API directly in either
  distribution mode; only the operator needs CRD/status/lease/configmap RBAC.
- Because the fallback path carries the same compiled JSON as the primary
  path, there is no "the ConfigMap disagrees with what routers saw over gRPC"
  class of bug to debug.

## Alternatives considered

- ConfigMap holding raw resource YAML, recompiled by the router: rejected;
  it would require every router replica to recompile identically (already
  true, since the compiler is pure) but doubles the amount of code that must
  agree with `routedctl` about compilation, and reintroduces the phase-2
  file-source code path as a hidden second compiler entry point instead of
  reusing it only for the standalone (no-operator) case it was built for.
- Mirroring every CRD field into protobuf messages for the gRPC service:
  rejected; it is a second schema for the same data that must be kept in
  lockstep with `routed-snapshot`'s types by hand.
- Leader-gating the gRPC serve path itself (only the leader answers watchers):
  rejected; it would make router snapshot delivery depend on leader election
  health for no correctness benefit, since every replica's compiled output is
  identical.
