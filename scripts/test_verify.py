import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("verify.py")
SPEC = importlib.util.spec_from_file_location("project_verify", SCRIPT)
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)
FAKE_CARGO = '''import json
import os
from pathlib import Path
import subprocess
import sys
import time

phase = Path(sys.argv[0]).name
if phase == "test":
    phase = "build" if "--no-run" in sys.argv else "test"

def record(event):
    data = {"phase": phase, "event": event, "time": time.monotonic(),
            "pid": os.getpid(), "args": sys.argv[1:],
            "settings": {key: os.environ[key] for key in
                         ("CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_INCREMENTAL", "CARGO_TARGET_DIR")}}
    with open(os.environ["EVENTS"], "a") as output:
        output.write(json.dumps(data) + "\\n")

record("start")
if os.environ.get("CHILD") == phase:
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    Path(os.environ["CHILD_PID"]).write_text(str(child.pid))
time.sleep(float(os.environ.get("SLEEP_" + phase.upper(), "0")))
record("end")
sys.exit(int(os.environ.get("EXIT_" + phase.upper(), "0")))
'''


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        self.script = self.root / "scripts/verify.py"
        shutil.copyfile(SCRIPT, self.script)
        (self.root / "scripts/test_fixture.py").write_text("import unittest\nclass Fixture(unittest.TestCase):\n    def test_fixture(self):\n        self.assertTrue(True)\n")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "cargo").symlink_to(sys.executable)
        for name in ("test", "fmt", "clippy"):
            (self.root / name).write_text(FAKE_CARGO)
        self.events = self.root / "events.jsonl"
        self.env = {**os.environ, "PATH": str(self.bin), "EVENTS": str(self.events),
                    "CARGO_TARGET_DIR": str(self.root / "target"),
                    "CARGO_PROFILE_DEV_DEBUG": "2", "CARGO_PROFILE_TEST_DEBUG": "2", "CARGO_INCREMENTAL": "1"}

    def start(self, *args, **env):
        process = subprocess.Popen([sys.executable, str(self.script), *args], cwd=self.root,
                                   env={**self.env, **env}, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.addCleanup(self.stop, process)
        return process

    def stop(self, process):
        if process.poll() is None:
            process.terminate()
        process.communicate(timeout=5)

    def result(self, process):
        output, _ = process.communicate(timeout=10)
        messages = [json.loads(line)["verify"] for line in output.splitlines() if line.startswith('{"verify":')]
        return process.returncode, messages, output

    def records(self):
        return [json.loads(line) for line in self.events.read_text().splitlines()] if self.events.exists() else []

    def wait_started(self, phase="build"):
        until = time.monotonic() + 5
        while time.monotonic() < until:
            if any(row["phase"] == phase and row["event"] == "start" for row in self.records()):
                return
            time.sleep(0.01)
        self.fail(f"{phase} did not start")

    def test_all_runs_checks_and_real_test_command_with_consistent_settings(self):
        code, messages, output = self.result(self.start("all"))
        self.assertEqual(code, 0, output)
        self.assertEqual([row["phase"] for row in messages if row.get("status") == "passed" and "phase" in row],
                         ["verifier-tests", "fmt", "build", "test", "clippy"])
        rows = [row for row in self.records() if row["event"] == "start"]
        self.assertEqual([row["phase"] for row in rows], ["fmt", "build", "test", "clippy"])
        for row in rows:
            for key in VERIFY.SETTINGS:
                self.assertEqual(row["settings"][key], "0")
        build, test = rows[1:3]
        self.assertEqual(build["args"][:-1], test["args"])
        self.assertEqual(build["args"][-1], "--no-run")
        self.assertIn("--offline", test["args"])
        self.assertIn("--locked", test["args"])
        self.assertEqual(test["args"][test["args"].index("--profile") + 1], "test")

    def test_failures_propagate_and_stop_without_retry(self):
        for phase, mode, expected in (("build", "test", ["build"]), ("test", "test", ["build", "test"]),
                                      ("fmt", "all", ["fmt"]), ("clippy", "clippy", ["clippy"])):
            with self.subTest(phase=phase):
                self.events.unlink(missing_ok=True)
                code, messages, output = self.result(self.start(mode, **{"EXIT_" + phase.upper(): "7"}))
                self.assertEqual(code, 7, output)
                self.assertEqual(messages[-1]["phase"], phase)
                self.assertEqual(messages[-1]["status"], "failed")
                self.assertEqual([row["phase"] for row in self.records() if row["event"] == "start"], expected)

    def test_build_and_test_have_independent_deadlines(self):
        code, messages, output = self.result(self.start("test", "--build-seconds", "0.6", "--test-seconds", "0.4",
                                                       SLEEP_BUILD="0.45", SLEEP_TEST="0.2"))
        self.assertEqual(code, 0, output)
        for phase in ("build", "test"):
            with self.subTest(phase=phase):
                code, messages, output = self.result(self.start("test", "--" + phase + "-seconds", "0.15",
                                                               **{"SLEEP_" + phase.upper(): "2"}))
                self.assertEqual(code, 124, output)
                self.assertEqual(messages[-1]["phase"], phase)
                self.assertEqual(messages[-1]["status"], "timeout")
                self.assertLess(messages[-1]["elapsed_seconds"], 1.5)

    def test_same_directory_and_symlink_serialize_entire_runs(self):
        target = self.root / "target"
        target.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(target, target_is_directory=True)
        first = self.start("test", SLEEP_BUILD="0.35", SLEEP_TEST="0.2")
        self.wait_started()
        second = self.start("test", "--target-dir", str(alias), "--build-seconds", "0.3")
        self.assertEqual(self.result(first)[0], 0)
        code, messages, output = self.result(second)
        self.assertEqual(code, 0, output)
        rows = self.records()
        self.assertEqual([(row["phase"], row["event"]) for row in rows],
                         [(phase, event) for phase in ("build", "test", "build", "test") for event in ("start", "end")])
        waited = next(row["elapsed_seconds"] for row in messages if row.get("status") == "acquired")
        self.assertGreater(waited, 0.3)
        self.assertEqual(messages[0]["target"], str(target))

    def test_distinct_directories_can_run_concurrently(self):
        first = self.start("fmt", SLEEP_FMT="0.5")
        self.wait_started("fmt")
        second = self.start("fmt", "--target-dir", str(self.root / "other"), SLEEP_FMT="0.2")
        self.assertEqual(self.result(first)[0], 0)
        self.assertEqual(self.result(second)[0], 0)
        self.assertEqual([row["event"] for row in self.records()], ["start", "start", "end", "end"])

    def test_lock_timeout_never_starts_cargo_and_failure_releases_lock(self):
        first = self.start("fmt", SLEEP_FMT="0.4", EXIT_FMT="9")
        self.wait_started("fmt")
        code, messages, output = self.result(self.start("test", "--lock-seconds", "0.1"))
        self.assertEqual(code, 124, output)
        self.assertEqual(messages[-1]["phase"], "lock")
        self.assertEqual(len(self.records()), 1)
        self.assertEqual(self.result(first)[0], 9)
        self.assertEqual(self.result(self.start("fmt", "--lock-seconds", "0.1"))[0], 0)

    def assert_dead(self, pid):
        until = time.monotonic() + 3
        while time.monotonic() < until:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            stat = Path(f"/proc/{pid}/stat")
            if stat.exists() and stat.read_text().split()[2] == "Z":
                return
            time.sleep(0.02)
        self.fail(f"child {pid} still running")

    def test_timeout_and_cancellation_stop_descendants_and_release_lock(self):
        for cancel in (False, True):
            with self.subTest(cancel=cancel):
                pid_file = self.root / "child.pid"
                pid_file.unlink(missing_ok=True)
                process = self.start("fmt", "--check-seconds", "0.4", SLEEP_FMT="60", CHILD="fmt", CHILD_PID=str(pid_file))
                until = time.monotonic() + 3
                while not pid_file.exists() and time.monotonic() < until:
                    time.sleep(0.01)
                self.assertTrue(pid_file.exists())
                if cancel:
                    process.send_signal(signal.SIGTERM)
                code, messages, output = self.result(process)
                self.assertEqual(code, 143 if cancel else 124, output)
                self.assertEqual(messages[-1]["phase"], "fmt")
                self.assert_dead(int(pid_file.read_text()))
                self.assertEqual(self.result(self.start("fmt", "--lock-seconds", "0.1"))[0], 0)

    def test_disk_warning_never_removes_files(self):
        target = self.root / "target"
        target.mkdir()
        sentinel = target / "precious"
        sentinel.write_text("retain me")
        code, messages, output = self.result(self.start("fmt", "--min-free-gib", "1000000"))
        self.assertEqual(code, 0, output)
        self.assertTrue(any(row.get("status") == "warning" and row["phase"] == "disk" for row in messages))
        self.assertEqual(sentinel.read_text(), "retain me")
        with patch.object(VERIFY.shutil, "disk_usage", return_value=shutil._ntuple_diskusage(1000, 950, 50)), patch.object(VERIFY, "emit") as emit:
            VERIFY.disk_warning(target, 0.000000001)
            self.assertEqual(emit.call_args.kwargs["status"], "warning")

    def test_plan_is_read_only_and_outer_budget_covers_all_phases(self):
        for mode in ("all", "test", "fmt", "clippy"):
            code, messages, output = self.result(self.start(mode, "--plan", "--online"))
            self.assertEqual(code, 0, output)
            plan = messages[0]
            self.assertGreaterEqual(plan["outer_timeout_seconds"], 600 + sum(row["seconds"] + 2 for row in plan["phases"]) + 60)
            for row in plan["phases"]:
                self.assertNotIn("--offline", row["command"])
            self.assertFalse((self.root / "target").exists())
        self.assertFalse(self.events.exists())

    def test_invalid_budgets_fail_before_work(self):
        for value in ("0", "-1", "nan", "inf"):
            code, _, output = self.result(self.start("--test-seconds", value))
            self.assertEqual(code, 2, output)
        self.assertFalse(self.events.exists())

    def test_killed_supervisor_keeps_lock_until_child_exits(self):
        first = self.start("fmt", SLEEP_FMT="0.6")
        self.wait_started("fmt")
        first.kill()
        code, messages, output = self.result(self.start("fmt", "--lock-seconds", "0.1"))
        self.assertEqual(code, 124, output)
        self.assertEqual(messages[-1]["phase"], "lock")
        self.assertEqual(self.result(first)[0], -signal.SIGKILL)
        self.assertEqual(self.result(self.start("fmt", "--lock-seconds", "0.1"))[0], 0)

    def test_verifier_test_failure_prevents_cargo(self):
        (self.root / "scripts/test_fixture.py").write_text("import unittest\nclass Fixture(unittest.TestCase):\n    def test_fixture(self):\n        self.fail('failure is required')\n")
        code, messages, output = self.result(self.start("test"))
        self.assertEqual(code, 1, output)
        self.assertEqual(messages[-1]["phase"], "verifier-tests")
        self.assertFalse(self.events.exists())

    def test_low_disk_warning_does_not_waive_failure(self):
        code, messages, output = self.result(self.start("fmt", "--min-free-gib", "1000000", EXIT_FMT="8"))
        self.assertEqual(code, 8, output)
        self.assertTrue(any(row.get("status") == "warning" for row in messages))
        self.assertEqual(messages[-1]["status"], "failed")
        self.assertIsNotNone(messages[-1]["phase_elapsed_seconds"])

    def test_missing_cargo_is_reported_with_phase(self):
        (self.bin / "cargo").unlink()
        code, messages, output = self.result(self.start("fmt"))
        self.assertEqual(code, 1, output)
        self.assertEqual(messages[-1]["phase"], "fmt")


class PolicyTests(unittest.TestCase):
    def test_committed_outer_deadlines_cover_runner_budgets(self):
        root = SCRIPT.parent.parent
        policy = (root / ".kyotoagent/closeout.yaml").read_text()
        self.assertIn("maxFailedAttemptsPerItem: 3", policy)
        self.assertIn("scope: task", policy)
        entries = re.findall(r"exec: \[python3, scripts/verify.py, (\w+)\]\n    timeoutSeconds: (\d+)", policy)
        self.assertEqual([mode for mode, _ in entries], ["test", "fmt", "clippy"])
        for mode, seconds in entries:
            with patch.object(sys, "argv", [str(SCRIPT), mode]):
                args = VERIFY.arguments()
            required = args.lock_seconds + sum(seconds + 2 for _, _, seconds in VERIFY.phases(args, root / "target")) + 60
            self.assertGreaterEqual(int(seconds), required)
        ci = (root / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/verify.py all --online", ci)
        with patch.object(sys, "argv", [str(SCRIPT), "all"]):
            args = VERIFY.arguments()
        required = args.lock_seconds + sum(seconds + 2 for _, _, seconds in VERIFY.phases(args, root / "target")) + 60
        minutes = int(re.search(r"timeout-minutes: (\d+)", ci)[1])
        self.assertGreater(minutes * 60, required)


if __name__ == "__main__":
    unittest.main()
