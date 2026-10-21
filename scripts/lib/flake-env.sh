# Sourced (not run) by the cargo/tool-bearing scripts so they run inside the flake dev shell
# (DECISIONS.md ADR-0010) however they are invoked: inside `nix develop`, under an allowed direnv,
# or from a plain shell. Order: already in the dev shell -> direnv -> re-exec the calling script
# under `nix develop`. Without nix, the script carries on with the ambient toolchain.
# Callers must not have consumed stdin before sourcing this (a re-exec would lose it).
if [ -z "${IN_NIX_SHELL:-}" ]; then
  eval "$(direnv export bash 2>/dev/null)" 2>/dev/null || true
fi
if [ -z "${IN_NIX_SHELL:-}" ] && [ -z "${UNIBLOX_FLAKE_REEXEC:-}" ] && command -v nix >/dev/null 2>&1; then
  export UNIBLOX_FLAKE_REEXEC=1
  exec nix develop --extra-experimental-features 'nix-command flakes' --command "$0" "$@"
fi
