#!/usr/bin/env python3
"""bench_pair.py must not report success for a run that lost something.

    python3 scripts/tests/test_bench_pair.py

No benchmark is built: `build` is replaced and the "benchmark binaries" are
shell scripts that print what criterion prints.
"""

import contextlib
import io
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import bench_pair  # noqa: E402

CASE = "throughput/monocoque-compio/push_pull/{}  time:   [1.0000 ms 1.1000 ms 1.2000 ms]"


def fake_binary(directory, name, body):
    path = Path(directory) / name
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


class PairExitStatus(unittest.TestCase):
    def run_pair(self, old_body, new_body, new_builds=True):
        with tempfile.TemporaryDirectory() as tmp:
            old = fake_binary(tmp, "throughput-old", old_body)
            new = fake_binary(tmp, "throughput-new", new_body)

            def build(tree, target, backend, bench):
                if target is None:
                    return new if new_builds else None
                return old

            out = io.StringIO()
            argv = ["bench_pair.py", "--against", "v0", "--rounds", "2"]
            with mock.patch.object(bench_pair, "WORK", Path(tmp) / "work"), \
                    mock.patch.object(bench_pair, "checkout", return_value=Path(tmp) / "v0"), \
                    mock.patch.object(bench_pair, "git", return_value="abc1234"), \
                    mock.patch.object(bench_pair, "build", side_effect=build), \
                    mock.patch.object(sys, "argv", argv), \
                    contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                try:
                    bench_pair.main()
                    status = 0
                except SystemExit as stop:
                    status = stop.code
            return status, out.getvalue()

    def test_a_clean_run_exits_zero(self):
        body = f"echo '{CASE.format(64)}'\n"
        status, out = self.run_pair(body, body)
        self.assertEqual(status, 0)
        self.assertNotIn("INCOMPLETE", out)

    def test_a_run_that_fails_partway_exits_non_zero(self):
        good = f"echo '{CASE.format(64)}'\necho '{CASE.format(256)}'\n"
        # Fails on its second round, after the first of its two cases.
        flaky = (f"echo '{CASE.format(64)}'\n"
                 "if [ -e ran-once ]; then exit 3; fi\n"
                 "touch ran-once\n"
                 f"echo '{CASE.format(256)}'\n")
        status, out = self.run_pair(good, flaky)
        self.assertEqual(status, 1)
        self.assertIn("INCOMPLETE", out)
        self.assertIn("the new run failed in round 2", out)
        self.assertIn("push_pull/256: 1 of 2 rounds on the new side", out)

    def test_a_benchmark_that_does_not_build_exits_non_zero(self):
        body = f"echo '{CASE.format(64)}'\n"
        status, out = self.run_pair(body, body, new_builds=False)
        self.assertEqual(status, 1)
        self.assertIn("skipped, it does not build in the working tree", out)

    def test_no_matching_case_exits_non_zero(self):
        status, out = self.run_pair("true\n", "true\n")
        self.assertEqual(status, 1)
        self.assertIn("no case matched", out)


class PinTopology(unittest.TestCase):
    """A laptop with two threads per performance core, then single-thread cores."""

    CORE_OF = {0: 0, 1: 0, 2: 1, 3: 1, 4: 2, 5: 3}

    def sysfs(self, tmp):
        for cpu, core in self.CORE_OF.items():
            topology = Path(tmp) / f"cpu{cpu}" / "topology"
            topology.mkdir(parents=True)
            (topology / "physical_package_id").write_text("0\n")
            (topology / "core_id").write_text(f"{core}\n")
        return Path(tmp)

    def test_two_threads_of_one_core_are_named(self):
        with tempfile.TemporaryDirectory() as tmp:
            sysfs = self.sysfs(tmp)
            self.assertEqual(bench_pair.cpus_sharing_a_core("2,3", sysfs), (2, 3))
            self.assertEqual(bench_pair.cpus_sharing_a_core("4,0-1", sysfs), (0, 1))

    def test_separate_cores_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            sysfs = self.sysfs(tmp)
            self.assertIsNone(bench_pair.cpus_sharing_a_core("0,2", sysfs))
            self.assertIsNone(bench_pair.cpus_sharing_a_core("4-5", sysfs))

    def test_an_unreadable_topology_claims_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            sysfs = self.sysfs(tmp)
            self.assertIsNone(bench_pair.cpus_sharing_a_core("0,9", sysfs))
            self.assertIsNone(bench_pair.cpus_sharing_a_core("zero,one", sysfs))


if __name__ == "__main__":
    unittest.main()
