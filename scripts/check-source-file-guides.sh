#!/usr/bin/env bash
# Verifies that maintained source files begin with a concise file-purpose comment.
set -euo pipefail

cd "$(dirname "$0")/.."

status=0
while IFS= read -r -d '' path; do
  case "$path" in
    *.rs)
      pattern='^//(!)? '
      line=1
      ;;
    *.nix)
      pattern='^# '
      line=1
      ;;
    *.sql)
      pattern='^-- '
      line=1
      ;;
    *.sh)
      pattern='^# '
      line=2
      ;;
    *)
      continue
      ;;
  esac

  if ! sed -n "${line}p" "$path" | grep -Eq "$pattern"; then
    printf 'missing file-purpose comment: %s\n' "$path" >&2
    status=1
  fi
done < <(
  find crates nix scripts tests/nixos \
    -path '*/target' -prune -o \
    -type f \( -name '*.rs' -o -name '*.nix' -o -name '*.sql' -o -name '*.sh' \) \
    -print0
)

exit "$status"
