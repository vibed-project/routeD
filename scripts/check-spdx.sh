#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# check-spdx.sh: every .rs/.sh/.py source file carries an SPDX identifier in its first 3 lines.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
while IFS= read -r f; do
  if ! head -n 3 "$f" | grep -q 'SPDX-License-Identifier: Apache-2.0'; then
    echo "missing SPDX header: $f"
    fail=1
  fi
done < <(find . -type f \( -name '*.rs' -o -name '*.sh' -o -name '*.py' \) \
  -not -path './target/*' -not -path './.cache/*' -not -path './.git/*' -not -path '*/.venv/*')
if [ "$fail" -eq 0 ]; then echo "ok: SPDX headers present"; fi
exit "$fail"
