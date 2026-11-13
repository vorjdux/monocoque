#!/usr/bin/env python3
"""Count the atomic instructions a binary executed, from a callgrind dump.

    count_atomics.py BINARY CALLGRIND_OUT [BASE_OUT N]

An atomic is an x86 instruction with a `lock` prefix, or `xchg` with a memory
operand (implicitly locked); plain loads and stores are not, even for atomic
types, since x86 orders them anyway. Only instructions inside BINARY count, so
the binary must be built non-PIE with symbols (see count_atomics.sh) for the
addresses callgrind records to match objdump's.

With BASE_OUT and N, prints the difference per unit of N instead, by function:
two runs that differ only in their message count cancel out setup.

The dump needs `--dump-instr=yes --dump-line=no`, so each cost line's
position is an instruction address.
"""

import re
import subprocess
import sys
from collections import defaultdict


def atomic_addresses(binary: str) -> set:
    out = subprocess.run(
        ["objdump", "-d", "--no-show-raw-insn", binary], capture_output=True, text=True, check=True
    ).stdout
    found = set()
    for line in out.splitlines():
        m = re.match(r"\s*([0-9a-f]+):\s+(.*)", line)
        if m and (m.group(2).startswith("lock ") or (m.group(2).startswith("xchg") and "(" in m.group(2))):
            found.add(int(m.group(1), 16))
    return found


def count(dump: str, atomics: set) -> dict:
    """Self cost of atomic instructions per function name."""
    names, by_id = {}, defaultdict(int)
    fn, last, skip_next = "?", 0, False
    with open(dump) as f:
        for line in f:
            line = line.rstrip("\n")
            # Function names are compressed: defined once, on either an fn= or
            # a cfn= line, then referred to by id.
            m = re.match(r"(c?fn)=\((\d+)\)(?: (.*))?", line)
            if m:
                if m.group(3):
                    names[m.group(2)] = m.group(3)
                if m.group(1) == "fn":
                    fn = m.group(2)
                continue
            if line.startswith("calls="):
                # The next cost line is the callee's inclusive cost.
                skip_next = True
                continue
            if not line or line[0] not in "0123456789+-*":
                continue
            parts = line.split()
            pos = parts[0]
            if pos == "*":
                addr = last
            elif pos[0] in "+-":
                delta = int(pos[1:], 0)
                addr = last + delta if pos[0] == "+" else last - delta
            else:
                addr = int(pos, 0)
            last = addr
            if skip_next:
                skip_next = False
                continue
            if addr in atomics and len(parts) > 1:
                by_id[fn] += int(parts[1])
    return {names.get(k, k): v for k, v in by_id.items()}


def main() -> None:
    binary, dump = sys.argv[1], sys.argv[2]
    atomics = atomic_addresses(binary)
    new = count(dump, atomics)
    if len(sys.argv) < 5:
        print(f"TOTAL {sum(new.values())}")
        for name, c in sorted(new.items(), key=lambda x: -x[1]):
            print(f"{c:>10}  {name}")
        return
    base, n = count(sys.argv[3], atomics), int(sys.argv[4])
    total = (sum(new.values()) - sum(base.values())) / n
    print(f"PER-UNIT {total:.2f}")
    rows = [((new.get(k, 0) - base.get(k, 0)) / n, k) for k in set(new) | set(base)]
    for v, name in sorted(rows, reverse=True):
        if abs(v) >= 0.05:
            print(f"{v:8.2f}  {name}")


if __name__ == "__main__":
    main()
