#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# cargo-in-podman.sh: run a Rust toolchain command inside the pinned routeD
# toolchain container. This development environment has no host Rust toolchain.
#
# Usage: scripts/cargo-in-podman.sh cargo build --workspace
#        scripts/cargo-in-podman.sh cargo test --workspace
#
# The image is derived from docker.io/library/rust:<channel>-bookworm (see
# build/toolchain.Containerfile) and built on first use. Override with
# ROUTED_RUST_IMAGE to use a different image.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUST_VERSION="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "${REPO_ROOT}/rust-toolchain.toml")"
IMAGE="${ROUTED_RUST_IMAGE:-localhost/routed-toolchain:${RUST_VERSION}}"

if [ -z "${ROUTED_RUST_IMAGE:-}" ] && ! podman image exists "${IMAGE}"; then
  echo "cargo-in-podman: building ${IMAGE}" >&2
  podman build -t "${IMAGE}" -f "${REPO_ROOT}/build/toolchain.Containerfile" "${REPO_ROOT}/build"
fi

mkdir -p "${REPO_ROOT}/.cache/cargo-registry" "${REPO_ROOT}/.cache/cargo-git" "${REPO_ROOT}/target"

TTY=()
if [ -t 1 ]; then TTY=(-t); fi

# Forward selected variables only when they are set (an empty-but-set RUST_LOG
# would silence logging).
PASSTHRU=()
for v in ROUTED_COMMIT RUST_LOG PROPTEST_CASES ROUTED_PERF ROUTED_PERF_SLACK UPDATE_GOLDEN ORT_DYLIB_PATH; do
  if [ -n "${!v:-}" ]; then PASSTHRU+=(-e "${v}=${!v}"); fi
done

exec podman run --rm ${TTY[@]+"${TTY[@]}"} \
  -v "${REPO_ROOT}:/src:Z" \
  -v "${REPO_ROOT}/.cache/cargo-registry:/usr/local/cargo/registry:Z" \
  -v "${REPO_ROOT}/.cache/cargo-git:/usr/local/cargo/git:Z" \
  -w /src \
  -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" \
  -e CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}" \
  ${PASSTHRU[@]+"${PASSTHRU[@]}"} \
  "${IMAGE}" "$@"
