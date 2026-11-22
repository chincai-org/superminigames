#!/usr/bin/env bash
# T-VER-4 / VER-18: every `requires` or `ensures` clause in `logic` must carry
# a requirement-ID comment on the same or previous line.
set -eu

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
LOGIC="$ROOT/crates/logic"

if ! command -v grep >/dev/null 2>&1; then
  echo "check-clause-ids: grep not found in PATH" >&2
  exit 1
fi

if [ ! -d "$LOGIC" ]; then
  echo "check-clause-ids: $LOGIC not found" >&2
  exit 1
fi

status=0
clause='(^|[^[:alnum:]_])(requires|ensures)([^[:alnum:]_]|$)'
req='(//|/\*).*[A-Z][A-Z0-9]*-[0-9]+'

if hits="$(grep -rnE --include='*.rs' "$clause" "$LOGIC")"; then
  :
else
  rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-clause-ids: grep failed (exit $rc) while scanning $LOGIC" >&2
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
    if printf '%s\n' "$text" | grep -qE "$req"; then
      same_ok=1
    fi
    if [ "$prev" -ge 1 ] && sed -n "${prev}p" "$file" | grep -qE "$req"; then
      prev_ok=1
    fi
    if [ "$same_ok" -eq 0 ] && [ "$prev_ok" -eq 0 ]; then
      echo "check-clause-ids: $file:$line: requires/ensures clause without a requirement-ID comment (VER-18)" >&2
      status=1
    fi
  done <<<"$hits"
fi

if [ "$status" -eq 0 ]; then
  echo "check-clause-ids: OK"
fi
exit "$status"