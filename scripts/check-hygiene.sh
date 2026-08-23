#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# check-hygiene.sh: the Rust toolchain pin must agree everywhere it is repeated.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
pin="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
[ -n "$pin" ] || { echo "hygiene: no channel in rust-toolchain.toml"; exit 1; }
minor="${pin%.*}"

for f in build/toolchain.Containerfile build/routed.Dockerfile build/routed-operator.Dockerfile; do
  [ -f "$f" ] || continue
  if ! grep -qE "FROM docker.io/library/rust:${minor}(\.[0-9]+)?-bookworm" "$f"; then
    echo "hygiene: $f does not use rust:${minor}-bookworm"
    fail=1
  fi
done

ci=.github/workflows/ci.yaml
if [ -f "$ci" ] && grep -q 'dtolnay/rust-toolchain@' "$ci"; then
  if ! grep -q "dtolnay/rust-toolchain@${pin}" "$ci"; then
    echo "hygiene: $ci rust-toolchain action is not pinned to ${pin}"
    fail=1
  fi
fi

if [ "$fail" -eq 0 ]; then echo "ok: toolchain pin ${pin} consistent"; fi
exit "$fail"
