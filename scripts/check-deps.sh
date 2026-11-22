#!/usr/bin/env bash
# CI-1 / T-WSP-1: enforce the dependency rules in spec 1.1 (WSP-1, WSP-2,
# WSP-6, WSP-9, PRO-4, STO-1, VER-11).
#
# cargo-deny cannot express "crate A may not depend on crate B" for
# third-party crates without also denying the crates that legitimately depend
# on B (tokio, serde, ...). This script checks the direct dependency edges of
# every workspace package using `cargo metadata --no-deps`.
#
# Normal dependencies are an ALLOWLIST, not a denylist: each workspace crate
# may only use the names in the "MAY depend on" column of the spec 1.1 table,
# so an unlisted crate of any kind (an I/O crate, a third-party helper, a
# workspace crate that points the wrong way) is a failure.
#
# Dev-dependencies and build-dependencies are constrained the same way: a
# dev/build dependency passes only if it is either
#   (a) generic test tooling from the shared set
#       {proptest, criterion, tempfile, serde_json}, or
#   (b) already allowed for that crate by its own spec 1.1 normal allowlist.
# So `engine` may keep its `tokio` dev-dependency (tokio is in engine's
# allowlist) while `logic`, `core`, `protocol`, `client` and every game crate
# may not; `uuid`, `rand_core` and `rand_chacha` follow each crate's own
# allowlist instead of a shared blanket permit. This is what keeps VER-11
# (logic MUST NOT depend on tokio) and WSP-9 (games MUST NOT depend on tokio)
# true once dev-dependencies are in play.
set -eu

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
META="$(mktemp)"
trap 'rm -f "$META"' EXIT

cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" >"$META"

python3 - "$META" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    metadata = json.load(fh)

GAME_PREFIX = "superminigames-games-"

# spec 1.1 "MAY depend on" column, one entry per workspace crate (WSP-2).
ALLOWED_NORMAL = {
    # verus only: `vstd` and `builtin` arrive with the Verus toolchain (VER-10),
    # not as Cargo dependencies, so logic has no normal dependency.
    "superminigames-logic": set(),
    "superminigames-core": {
        "superminigames-logic",
        "serde",
        "serde_bytes",
        "thiserror",
        "uuid",
        "rand_core",
        "rand_chacha",
    },
    "superminigames-protocol": {
        "superminigames-core",
        "serde",
        "serde_bytes",
        "rmp-serde",
        "uuid",
        "serde_json",  # only behind the optional `json` debug feature
    },
    "superminigames-engine": {
        "superminigames-core",
        "superminigames-logic",
        "tokio",  # sync, time, macros, rt only
        "rand_core",
        "rand_chacha",
        "thiserror",
        "tracing",
        "serde",
    },
    "superminigames-server": {
        "superminigames-engine",
        "superminigames-protocol",
        "superminigames-core",
        "axum",
        "rustls",
        "tokio",
        "sqlx",
        "tracing",
        "uuid",
        "chrono",
        "rand",
        "argon2",
        "sha2",
        "lettre",
    },
    # spec 1.1: game crates are allowed too (UIX-9); added below once the
    # workspace package names are known.
    "superminigames-client": {"superminigames-protocol", "superminigames-core"},
}

# spec 1.1: one crate per game, depending on `core` and the RNG crates only.
ALLOWED_NORMAL_GAME = {"superminigames-core", "rand_core", "rand_chacha"}

# Generic dev/build test tooling that any crate may add (see the header comment).
SHARED_TOOLING = {
    "proptest",
    "criterion",
    "tempfile",
    "serde_json",
}


def is_game(name):
    return name.startswith(GAME_PREFIX)


package_names = [package["name"] for package in metadata["packages"]]
game_crates = {name for name in package_names if is_game(name)}
ALLOWED_NORMAL["superminigames-client"] |= game_crates

failures = []
checked = 0
for package in metadata["packages"]:
    parent = package["name"]
    if is_game(parent):
        allowed_normal = ALLOWED_NORMAL_GAME
    elif parent in ALLOWED_NORMAL:
        allowed_normal = ALLOWED_NORMAL[parent]
    else:
        failures.append(
            f"{parent}: no allowlist; add this crate's spec 1.1 "
            f"'MAY depend on' row to ALLOWED_NORMAL in scripts/check-deps.sh"
        )
        continue
    for dependency in package.get("dependencies", []):
        # `name` is the real package name; `rename` holds the local alias, so a
        # renamed dependency is still checked against its real package.
        child = dependency["name"]
        kind = dependency.get("kind") or "normal"
        edge = f"{parent} -> {child}"
        if dependency.get("rename"):
            edge += f" (imported as {dependency['rename']})"
        checked += 1
        if is_game(parent) and is_game(child):
            failures.append(
                f"{edge} ({kind}): WSP-9: game crates must not depend on each other; "
                f"each game owns its rules and the Game trait is the only shared interface"
            )
        elif kind in ("dev", "build"):
            if child not in SHARED_TOOLING and child not in allowed_normal:
                may = ", ".join(sorted(allowed_normal))
                rule = (
                    f"{parent} MAY depend only on {may}"
                    if may
                    else f"{parent} MAY not depend on any crate"
                )
                failures.append(
                    f"{edge} ({kind}): spec 1.1: {rule}; a dev/build dependency must "
                    f"also be generic test tooling "
                    f"({', '.join(sorted(SHARED_TOOLING))}); {child} is not allowed"
                )
        elif child not in allowed_normal:
            may = ", ".join(sorted(allowed_normal))
            rule = (
                f"{parent} MAY depend only on {may}"
                if may
                else f"{parent} MAY not depend on any crate"
            )
            failures.append(f"{edge} ({kind}): spec 1.1: {rule}; {child} is not allowed")

if failures:
    print("check-deps: dependency allowlist violations (spec 1.1):", file=sys.stderr)
    for failure in failures:
        print(f"  error: {failure}", file=sys.stderr)
    sys.exit(1)

print(
    f"check-deps: OK ({len(metadata['packages'])} workspace packages, "
    f"{checked} direct dependencies checked against the spec 1.1 allowlist)"
)
PY
