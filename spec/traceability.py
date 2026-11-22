"""Generate spec/TRACEABILITY.md from spec/SPEC.md (VER-40 to VER-43).

python3 spec/traceability.py          write TRACEABILITY.md; exit 1 if a MUST requirement is uncovered
python3 spec/traceability.py --check  write nothing; exit 1 if uncovered or TRACEABILITY.md is stale
"""

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC, OUT = HERE / "SPEC.md", HERE / "TRACEABILITY.md"


def order(i):
    return (i.split("-")[0], int(i.split("-")[1]))


def ids(text):
    return re.findall(r"\b[A-Z]+-\d+\b", text)


def build(s):
    reqs = {
        m.group(1): m.group(2)
        for m in re.finditer(r"^- ([A-Z]+-\d+): (.*(?:\n  .*)*)", s, re.MULTILINE)
    }
    tests = {
        m.group(1): m.group(2)
        for m in re.finditer(r"^- (T-[A-Z]+-\d+)(?: \([^)]*\))?: (.*)", s, re.MULTILINE)
    }
    ci = {}
    for m in re.finditer(r"^\| (CI-\d+) \|.*\| ([^|]*) \|$", s, re.MULTILINE):
        for r in ids(m.group(2)):
            ci.setdefault(r, []).append(m.group(1))
    ver42 = re.search(r"^- VER-42:(.*)$", s, re.MULTILINE)
    if ver42 is None:
        sys.exit("SPEC.md has no VER-42 line (review-only requirements)")
    review = set(ids(ver42.group(1)))
    by_test = {}
    for t, text in tests.items():
        for r in ids(text):
            if r in reqs:
                by_test.setdefault(r, []).append(t)
    must = sorted(
        (r for r, text in reqs.items() if re.search(r"\bMUST\b", text)), key=order
    )
    rows, missing = [], []
    for r in must:
        how = []
        if r in by_test:
            how.append("tests: " + ", ".join(by_test[r]))
        if r in ci:
            how.append("CI: " + ", ".join(ci[r]))
        if r in review:
            how.append("review")
        if not how:
            missing.append(r)
            how = ["**UNCOVERED**"]
        rows.append(f"| {r} | {'; '.join(how)} |")
    doc = [
        "# Traceability (generated, do not edit)",
        "",
        "Generated from `spec/SPEC.md` by `spec/traceability.py`.",
        f"MUST requirements: {len(must)}. Uncovered: {len(missing)}.",
        "",
        "| Requirement | Verified by |",
        "|---|---|",
    ] + rows
    return "\n".join(doc) + "\n", missing, len(must)


def main():
    check = "--check" in sys.argv
    doc, missing, n = build(SPEC.read_text(encoding="utf-8"))
    stale = (not OUT.exists()) or OUT.read_text(encoding="utf-8") != doc
    if not check:
        OUT.write_text(doc, encoding="utf-8")
        stale = False
    print(
        f"{n} MUST requirements, {len(missing)} uncovered"
        + (f": {' '.join(missing)}" if missing else "")
    )
    if stale:
        print("TRACEABILITY.md is stale: run `python3 spec/traceability.py`")
    sys.exit(1 if (missing or stale) else 0)


if __name__ == "__main__":
    main()
