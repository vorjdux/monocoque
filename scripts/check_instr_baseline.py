#!/usr/bin/env python3
"""Compare instr_hotpath instruction counts with the committed baseline.

    cargo bench --bench instr_hotpath --features zmq -p monocoque-rs | tee out.txt
    scripts/check_instr_baseline.py out.txt            # fail on a regression
    scripts/check_instr_baseline.py out.txt --update   # rewrite the baseline

Fails if a bench rises more than LIMIT_PERCENT above the baseline, or if a bench
is missing on either side, so a run can never pass by having nothing to compare.
"""

import json
import re
import sys
from pathlib import Path

BASELINE = Path(__file__).resolve().parent.parent / "docs/perf/instr-baseline.json"
LIMIT_PERCENT = 5.0

INSTRUCTIONS = re.compile(r"^\s+Instructions:\s+(\d+)\|")


def parse(text):
    counts = {}
    name = None
    for line in text.splitlines():
        if line.startswith("instr_hotpath::"):
            name = line.strip()
        elif name and (m := INSTRUCTIONS.match(line)):
            counts[name] = int(m.group(1))
            name = None
    return counts


def main():
    args = [a for a in sys.argv[1:] if a != "--update"]
    if len(args) != 1:
        sys.exit(__doc__)
    counts = parse(Path(args[0]).read_text())
    if not counts:
        sys.exit(f"no instruction counts found in {args[0]}")
    if "--update" in sys.argv:
        BASELINE.write_text(json.dumps(counts, indent=2, sort_keys=True) + "\n")
        print(f"wrote {len(counts)} counts to {BASELINE}")
        return
    baseline = json.loads(BASELINE.read_text())
    failed = False
    for name in sorted(baseline.keys() | counts.keys()):
        old, new = baseline.get(name), counts.get(name)
        if old is None or new is None:
            print(f"FAIL {name}: only in {'the run' if old is None else 'the baseline'}")
            failed = True
            continue
        change = (new - old) * 100.0 / old
        over = change > LIMIT_PERCENT
        failed |= over
        print(f"{'FAIL' if over else 'ok  '} {name}: {old} -> {new} ({change:+.2f}%)")
    if failed:
        sys.exit("instruction counts differ; if intended, rerun with --update and commit")


if __name__ == "__main__":
    main()
