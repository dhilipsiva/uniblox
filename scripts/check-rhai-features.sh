#!/usr/bin/env bash
# The sandbox depends on two rhai features staying OFF (ADR-0011, ADR-0046): `unchecked` compiles out
# operation counting and every size and depth limit, and `internals` exposes engine internals. Fail if
# either is resolved for any target, directly or through another crate. Run by `just ci`.
set -euo pipefail
cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"

features="$(cargo tree --locked -e features -i rhai --target all --prefix none --format '{p} {f}' 2>/dev/null)"
if [ -z "${features}" ]; then
  echo "FAILED: could not resolve rhai's features (is rhai still a dependency?)" >&2
  exit 1
fi
if printf '%s\n' "${features}" | grep -Eq 'rhai feature "(unchecked|internals)"|^rhai v[^ ]+ .*\b(unchecked|internals)\b'; then
  echo "FAILED: rhai's unchecked or internals feature is enabled; that voids the sandbox." >&2
  printf '%s\n' "${features}" | grep -E 'unchecked|internals' >&2
  exit 1
fi
echo "rhai sandbox features ok (neither unchecked nor internals)"
