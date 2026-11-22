#!/usr/bin/env bash
# T-VER-1 / VER-17: fail on Verus escape hatches in `logic` without an
# APPROVED-BY: marker on the same or previous line.
set -eu

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
LOGIC="$ROOT/crates/logic"

if ! command -v grep >/dev/null 2>&1; then
  echo "check-verus-escapes: grep not found in PATH" >&2
  exit 1
fi

if [ ! -d "$LOGIC" ]; then
  echo "check-verus-escapes: $LOGIC not found" >&2
  exit 1
fi

status=0
# Word boundaries: do not match external_body as external.
pattern='(^|[^[:alnum:]_])(assume|admit|external_body|external)([^[:alnum:]_]|$)'

if hits="$(grep -rnE --include='*.rs' "$pattern" "$LOGIC")"; then
  :
else
  rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-verus-escapes: grep failed (exit $rc) while scanning $LOGIC" >&2
    exit 1
  fi
  hits=""
fi

if [ -n "$hits" ]; then
  while IFS=: read -r file line _rest; do
    text="$(sed -n "${line}p" "$file")"
    trimmed="${text#"${text%%[![:space:]]*}"}"
    if [[ "$trimmed" == //* || "$trimmed" == /\** || "$trimmed" == \** ]]; then
      continue
    fi
    prev=$((line - 1))
    same_ok=0
    prev_ok=0
    if printf '%s\n' "$text" | grep -q 'APPROVED-BY:'; then
      same_ok=1
    fi
    if [ "$prev" -ge 1 ] && sed -n "${prev}p" "$file" | grep -q 'APPROVED-BY:'; then
      prev_ok=1
    fi
    if [ "$same_ok" -eq 0 ] && [ "$prev_ok" -eq 0 ]; then
      echo "check-verus-escapes: $file:$line: Verus escape hatch without an APPROVED-BY: marker (VER-17)" >&2
      status=1
    fi
  done <<<"$hits"
fi

if [ "$status" -eq 0 ]; then
  echo "check-verus-escapes: OK"
fi
exit "$status"