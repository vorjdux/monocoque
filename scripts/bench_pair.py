#!/usr/bin/env python3
"""Compare the working tree with another revision in alternating rounds.

    scripts/bench_pair.py --against v0.4.2
    scripts/bench_pair.py --against v0.4.2 --backend all --pin 0,2
    scripts/bench_pair.py --against main --bench throughput --filter 'push_pull/64$'
    scripts/bench_pair.py --against HEAD --bench latency --bench pipelined_throughput

A saved criterion baseline is measured at one time and compared with a run made
at another, so whatever changed on the machine in between is counted as a
difference in the code. This script builds both revisions first and then runs
them turn about, old then new, new then old, so both see the same machine. It
reports the median of the rounds for each case and the range they covered: a
wide range means the case is unstable and its median deserves less trust.

The other revision is checked out into a git worktree under target/bench-pair/
and built into its own target directory, so the working tree is not touched.
A case that exists in only one revision is listed without a comparison.

A full benchmark takes minutes per round. Use --filter to pick cases and
--rounds to trade time for confidence. --secs shortens the benchmarks that
read BENCH_SECS; a revision whose benchmark does not read it runs at its own
length, which lengthens the run without changing what is measured.
"""

import argparse
import os
import re
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "bench-pair"
PACKAGE = "monocoque-rs"

FEATURES = {
    "compio": ["--features", "zmq"],
    "tokio": ["--no-default-features", "--features", "runtime-tokio,zmq"],
    "smol": ["--no-default-features", "--features", "runtime-smol,zmq"],
}

# A sender and a receiver, one thread each: these can be pinned to two cores.
TWO_THREAD = {"latency", "throughput", "pipelined_throughput", "ipc_vs_tcp"}

ANSI = re.compile(r"\x1b\[[0-9;]*m")
TIME = re.compile(r"^(.*?)\s*time:\s+\[\S+ \S+ (\S+) (\S+) \S+ \S+\]")
SECONDS = {"ps": 1e-12, "ns": 1e-9, "µs": 1e-6, "us": 1e-6, "ms": 1e-3, "s": 1.0}


def git(*args, cwd=ROOT):
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def checkout(ref):
    """A worktree of `ref`, created or moved to it. Returns its directory."""
    commit = git("rev-parse", "--verify", f"{ref}^{{commit}}")
    tree = WORK / re.sub(r"[^A-Za-z0-9._-]", "_", ref)
    if not (tree / ".git").exists():
        WORK.mkdir(parents=True, exist_ok=True)
        git("worktree", "prune")
        git("worktree", "add", "--detach", str(tree), commit)
    elif git("rev-parse", "HEAD", cwd=tree) != commit:
        git("checkout", "--detach", commit, cwd=tree)
    return tree


def build(tree, target, backend, bench):
    """Build one benchmark. Returns the binary, or None if it does not build."""
    env = dict(os.environ)
    if target:
        env["CARGO_TARGET_DIR"] = str(target)
    cmd = ["cargo", "bench", "-p", PACKAGE, *FEATURES[backend], "--bench", bench, "--no-run"]
    done = subprocess.run(cmd, cwd=tree, env=env, capture_output=True, text=True)
    found = re.search(r"\((\S*deps[/\\]" + re.escape(bench) + r"-[0-9a-f]+(?:\.exe)?)\)", done.stderr)
    if done.returncode != 0 or not found:
        tail = "\n".join(done.stderr.strip().splitlines()[-6:])
        print(f"  {bench} does not build in {tree}:\n{tail}", file=sys.stderr)
        return None
    binary = Path(found.group(1))
    return binary if binary.is_absolute() else Path(tree) / binary


def measure(binary, pattern, pin, scratch, env):
    """Run one benchmark binary. Returns {case: seconds per iteration}."""
    cmd = [str(binary), "--bench", pattern, "--noplot"]
    if pin:
        cmd = ["taskset", "-c", pin, *cmd]
    scratch.mkdir(parents=True, exist_ok=True)
    # Criterion keeps its own history under the target directory. Give each
    # side a scratch one, so neither the working tree's saved baselines nor
    # the other side's history is written to.
    env = dict(env, CARGO_TARGET_DIR=str(scratch))
    done = subprocess.run(cmd, cwd=scratch, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        tail = "\n".join((done.stdout + done.stderr).strip().splitlines()[-8:])
        sys.exit(f"{binary} failed:\n{tail}")
    lines = ANSI.sub("", done.stdout).splitlines()
    times = {}
    for i, line in enumerate(lines):
        hit = TIME.match(line)
        if not hit:
            continue
        # A long case name is printed on the line above its time.
        name = hit.group(1).strip() or lines[i - 1].strip()
        times[name] = float(hit.group(2)) * SECONDS[hit.group(3)]
    return times


def show(seconds):
    for unit, size in (("s", 1.0), ("ms", 1e-3), ("µs", 1e-6), ("ns", 1e-9)):
        if seconds >= size:
            return f"{seconds / size:.4g} {unit}"
    return f"{seconds / 1e-12:.4g} ps"


def report(old, new, old_name):
    width = max((len(n) for n in [*old, *new]), default=20)
    print(f"  {'case':{width}}  {old_name + ' median':>16}  {'new median':>12}  "
          f"{'new is':>8}   ranges (old | new)")
    for name in sorted(set(old) & set(new), key=list(new).index):
        a, b = statistics.median(old[name]), statistics.median(new[name])
        change = (a / b - 1) * 100
        word = "faster" if change >= 0 else "slower"
        print(f"  {name:{width}}  {show(a):>16}  {show(b):>12}  {abs(change):5.1f}% {word}   "
              f"{show(min(old[name]))} to {show(max(old[name]))} | "
              f"{show(min(new[name]))} to {show(max(new[name]))}")
    for name in new:
        if name not in old:
            print(f"  {name:{width}}  {'not in ' + old_name:>16}  {show(statistics.median(new[name])):>12}")
    for name in old:
        if name not in new:
            print(f"  {name:{width}}  {show(statistics.median(old[name])):>16}  {'not in new':>12}")


def main():
    args = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    args.add_argument("--against", required=True, metavar="REF", help="tag, branch or commit to compare with")
    args.add_argument("--backend", default="compio", choices=[*FEATURES, "all"])
    args.add_argument("--bench", action="append", metavar="NAME",
                      help="benchmark to run, may be repeated (default: throughput)")
    args.add_argument("--filter", default="", metavar="REGEX",
                      help="only the cases matching this, within the backend's own cases")
    args.add_argument("--rounds", type=int, default=5, help="rounds per revision (default 5)")
    args.add_argument("--pin", metavar="CORES",
                      help="pin the two-thread benchmarks to these cores, for example 0,2")
    args.add_argument("--secs", type=int, metavar="N",
                      help="measurement seconds for benchmarks that read BENCH_SECS")
    opt = args.parse_args()

    backends = list(FEATURES) if opt.backend == "all" else [opt.backend]
    benches = opt.bench or ["throughput"]
    if opt.rounds < 1:
        sys.exit("--rounds must be at least 1")
    if opt.pin and subprocess.run(["taskset", "-c", opt.pin, "true"], capture_output=True).returncode != 0:
        sys.exit(f"cannot pin to cores {opt.pin} (is taskset installed?)")

    env = dict(os.environ)
    if opt.secs:
        env["BENCH_SECS"] = str(opt.secs)

    old_tree = checkout(opt.against)
    old_target = WORK / (old_tree.name + "-target")
    print(f"old: {opt.against} ({git('rev-parse', '--short', 'HEAD', cwd=old_tree)}) in {old_tree}")
    print(f"new: working tree at {git('rev-parse', '--short', 'HEAD')}"
          f"{' with uncommitted changes' if git('status', '--porcelain') else ''}")
    print(f"rounds: {opt.rounds}, pinned to: {opt.pin or 'not pinned'}\n")

    for backend in backends:
        for bench in benches:
            print(f"== {bench} on {backend}", flush=True)
            old_bin = build(old_tree, old_target, backend, bench)
            new_bin = build(ROOT, None, backend, bench)
            if not old_bin or not new_bin:
                print("  skipped\n")
                continue
            pattern = f"monocoque-{backend}/.*{opt.filter}" if opt.filter else f"monocoque-{backend}/"
            pin = opt.pin if bench in TWO_THREAD else None
            old, new = {}, {}
            sides = [(old_bin, old, WORK / "run-old"), (new_bin, new, WORK / "run-new")]
            for i in range(opt.rounds):
                # Swap who goes first each round, so neither side always runs
                # on a machine the other has just warmed up.
                for binary, into, scratch in sides if i % 2 == 0 else reversed(sides):
                    for name, t in measure(binary, pattern, pin, scratch, env).items():
                        into.setdefault(name, []).append(t)
                print(f"  round {i + 1} of {opt.rounds} done", flush=True)
            if not old and not new:
                print("  no case matched\n")
                continue
            report(old, new, opt.against)
            print(flush=True)


if __name__ == "__main__":
    main()
