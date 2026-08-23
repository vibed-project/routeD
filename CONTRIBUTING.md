# Contributing to routeD

Thank you for your interest. This guide covers the mechanics; see
`docs/architecture.md` and `docs/adr/` for the why.

## Prerequisites

routeD is written in Rust, but **you do not need a host Rust toolchain**: every
`cargo` command runs inside a pinned container through
`scripts/cargo-in-podman.sh` (podman 5+). If you do have a matching host
toolchain (`rust-toolchain.toml`), run `make <target> CARGO=cargo`.

Other tools used by `make ci`: `helm` (chart lint), GNU make.

## Building and testing

```sh
make build        # cargo build --workspace --locked
make test         # cargo test --workspace --locked
make lint         # cargo fmt --check + cargo clippy -D warnings
make deny         # cargo deny (licenses, advisories, bans)
make hygiene      # SPDX headers, crate boundaries, toolchain pin consistency
make crd-gen      # regenerate config/crd and charts/routed/crds from crates/api (CI checks drift)
make helm-lint    # helm lint + template
make ci           # everything GitHub Actions runs
```

The first container run builds `localhost/routed-toolchain:<version>` and
fetches crates; later runs are incremental (`target/` and the cargo registry
are cached under the repository).

## Code style

- `cargo fmt` and `cargo clippy --all-targets -D warnings` must pass.
- Every source file (`.rs`, `.sh`, `.py`) starts with
  `// SPDX-License-Identifier: Apache-2.0` (or `#` for scripts, after the shebang).
- `crates/decision`, `crates/policy`, `crates/snapshot`, and `crates/api` are
  pure: no tokio, kube client, network, or ONNX dependencies. `make boundary` enforces it.
- Prefer `BTreeMap` over `HashMap` wherever output must be deterministic.
- Tests are table-driven; behaviour that must never change (security ordering,
  header trust) has property tests.

## Commits and pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, ...).
- Sign off every commit (`git commit -s`) to certify the Developer Certificate
  of Origin in `DCO`.
- Run `make ci` before opening a pull request.
- Add or update an ADR in `docs/adr/` when a change establishes or alters a
  contract another component depends on. Ordinary implementation details do
  not need an ADR.

## Getting help

Open a GitHub discussion or issue. Security issues: see `SECURITY.md`.
