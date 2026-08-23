#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# check-crate-boundary.sh: enforce the architectural seams documented in
# docs/adr/0004-extension-seams.md and docs/adr/0005-workspace-layout-and-containerized-toolchain.md.
#
#  1. Pure crates (api, snapshot, policy, decision) must not depend on runtime/I-O crates
#     (kube with default-features = false is allowed in api for the CRD derive macros only).
#  2. No dependency may point outside the workspace via a relative path.
#  3. No leftovers from the earlier Go plan (routeD is Rust-only).
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0

pure_crates="crates/api crates/snapshot crates/policy crates/decision"
forbidden='^(tokio|kube-client|kube-runtime|axum|hyper|hyper-util|tonic|ort|reqwest|redis)\b'
for c in $pure_crates; do
  if grep -E "$forbidden" "$c/Cargo.toml" | grep -v '^#' >/dev/null; then
    echo "boundary: $c must stay pure (no runtime/I-O crates):"
    grep -nE "$forbidden" "$c/Cargo.toml"
    fail=1
  fi
done

if grep -rE 'path *= *"\.\./\.\.' --include=Cargo.toml cmd crates 2>/dev/null; then
  echo "boundary: path dependencies must not leave the workspace"
  fail=1
fi

if find . \( -name '*.go' -o -name go.mod -o -name go.sum \) -not -path './.cache/*' -not -path './target/*' | grep -q .; then
  echo "boundary: Go sources found; routeD is Rust-only"
  fail=1
fi

if [ "$fail" -eq 0 ]; then echo "ok: crate boundaries respected"; fi
exit "$fail"
