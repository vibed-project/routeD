# ADR-0005: Cargo workspace layout and containerized toolchain

## Status

Accepted

## Context

The spec mandates a repository layout (`cmd/`, `api/v1alpha1`, `internal/*`,
`config/`, `charts/`, `trainer/`, `docs/`, `examples/`, `test/e2e`) that was
written for Go. The development environment has no host Rust toolchain and
builds inside podman containers; CI uses real toolchains on GitHub runners.

## Decision

- One Cargo workspace (`resolver = "3"`, edition 2024, toolchain pinned in
  `rust-toolchain.toml`, `Cargo.lock` committed, `--locked` everywhere).
- **One crate per architectural seam**, mirroring the spec's `internal/*`
  names under `crates/` (`routed-decision`, `routed-policy`, ...). The cargo
  dependency graph is the enforcement of the seams: `routed-decision`,
  `routed-policy`, `routed-snapshot`, and `routed-api` are pure and may not
  depend on tokio, kube client/runtime, axum, hyper, tonic, ort, reqwest, or redis (`crates/api` uses `kube` with default features off, for the CRD derive macros only).
- Binaries live under `cmd/` exactly as in the spec. `api/v1alpha1` maps to
  `crates/api/src/v1alpha1`. `trainer/` is a `uv` Python project outside the
  workspace.
- Local builds run through `scripts/cargo-in-podman.sh`, which builds a
  toolchain image from `build/toolchain.Containerfile` (official
  `rust:<pin>-bookworm` plus clippy, rustfmt, cargo-deny) and bind-mounts
  `target/` and the cargo registry from the repository so rebuilds are
  incremental and the VM disk is not consumed. The Makefile exposes
  `CARGO ?=` so CI sets `CARGO=cargo`.
- `scripts/check-hygiene.sh` asserts that the toolchain pin is identical in
  `rust-toolchain.toml`, the Containerfiles, and the CI workflow.
- Runtime images are `gcr.io/distroless/cc-debian12:nonroot` for every binary
  (ADR-0002). Multi-arch images are produced by native runners per
  architecture and joined with a manifest list; no QEMU emulation.
- Build metadata (`version`, `commit`) comes from `crates/version` with a
  `build.rs` that reads `ROUTED_COMMIT` (set by the Makefile and image builds)
  or git.

## Consequences

- Contributors with podman but no Rust can build and test; contributors with a
  matching host toolchain are not slowed down.
- Adding a dependency to a pure crate fails `make boundary`.
- `cargo-deny` enforces the license allowlist (Apache-2.0 compatible) and bans
  `openssl-sys` (rustls only).

## Alternatives considered

- **Fewer, larger crates.** Rejected: seams would be comments rather than
  compile-time facts, and incremental builds would be slower.
- **sccache / remote cache.** Deferred: a persisted `target/` is sufficient.
- **QEMU multi-arch builds.** Rejected: Rust + ONNX Runtime under emulation is
  too slow and fragile.
