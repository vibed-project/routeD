# ADR-0021: Mutual TLS for snapshot distribution

## Status

Accepted (2026-08-23)

## Context

The operator's `SnapshotService` (ADR-0014) streams the compiled routing
snapshot to routers over gRPC. Inside a single trusted cluster network a
NetworkPolicy adequately fences the port, and the threat model recommended
exactly that. Two situations need more: clusters where the network is not
considered a boundary (shared nodes, no CNI policy support), and routers
consuming snapshots across a network boundary from a remote snapshot
source. In both, the stream needs server authentication (routers must not
accept snapshots from an impostor) and client authentication (snapshot
sources must not serve arbitrary peers).

## Decision

Mutual TLS, off by default, configured symmetrically by directory:

- Both sides read `tls.crt`/`tls.key` (own identity) and `ca.crt` (the CA
  that signed the peer) from one directory - the layout of a Kubernetes
  TLS secret plus a CA key, so cert-manager output mounts directly.
- Operator: `--snapshot-tls-dir` serves TLS and **requires** client
  certificates signed by `ca.crt`. Router: `--snapshot-tls-dir` (plus
  optional `--snapshot-tls-domain` when the dialled address differs from
  the certificate name) verifies the server and presents its identity.
- The helpers live in `routed-proto::tls`, so any snapshot source
  implementing the same proto offers the same configuration surface.
- No TLS directory means plain TCP, exactly as before: in-cluster installs
  with NetworkPolicy fencing keep working unchanged, and there is no
  self-signed fallback that would fake security.

Certificate rotation is by pod restart (secrets are re-read at startup
only); in-process reload is deliberately out of scope until someone needs
it.

## Consequences

- Cross-boundary snapshot distribution is possible without a service mesh.
- The threat-model gap "mTLS / authn for the snapshot stream" closes; the
  NetworkPolicy recommendation remains for plain-TCP installs.
- Operating cost: issuing and rotating a CA plus two leaf certificates,
  which cert-manager automates.
