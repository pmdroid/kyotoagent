import argparse
import fcntl
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
SETTINGS = {
    "CARGO_PROFILE_DEV_DEBUG": "0",
    "CARGO_PROFILE_TEST_DEBUG": "0",
    "CARGO_INCREMENTAL": "0",
}


class VerificationFailure(Exception):
    def __init__(self, phase, status, code, detail):
        self.phase = phase
        self.status = status
        self.code = code
        self.detail = detail
        self.elapsed_seconds = None


def emit(**fields):
    print(json.dumps({"verify": fields}), flush=True)


def positive_seconds(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("expected finite seconds greater than zero")
    return number


def arguments():
    parser = argparse.ArgumentParser(description="Serialized, phase-budgeted project verification")
    parser.add_argument("mode", choices=("all", "test", "fmt", "clippy"), nargs="?", default="all")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--online", action="store_true")
    parser.add_argument("--lock-seconds", type=positive_seconds, default=600)
    parser.add_argument("--build-seconds", type=positive_seconds, default=900)
    parser.add_argument("--test-seconds", type=positive_seconds, default=180)
    parser.add_argument("--check-seconds", type=positive_seconds, default=120)
    parser.add_argument("--min-free-gib", type=positive_seconds, default=5)
    parser.add_argument("--plan", action="store_true")
    return parser.parse_args()


def target_directory(args):
    target = args.target_dir or Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    return target.resolve()


def phases(args, target):
    common = ["--locked", "--target-dir", str(target), "--profile", "test"]
    if not args.online:
        common.append("--offline")
    commands = {
        "verifier-tests": ([sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"], args.check_seconds),
        "fmt": (["cargo", "fmt", "--check"], args.check_seconds),
        "build": (["cargo", "test", *common, "--no-run"], args.build_seconds),
        "test": (["cargo", "test", *common], args.test_seconds),
        "clippy": (["cargo", "clippy", "--all-targets", *common, "--", "-D", "warnings"], args.build_seconds),
    }
    names = {
        "all": ["verifier-tests", "fmt", "build", "test", "clippy"],
        "test": ["verifier-tests", "build", "test"],
        "fmt": ["fmt"],
        "clippy": ["clippy"],
    }[args.mode]
    return [(name, *commands[name]) for name in names]


def disk_warning(target, minimum):
    existing = target
    while not existing.exists():
        existing = existing.parent
    usage = shutil.disk_usage(existing)
    free_gib = usage.free / 1024 ** 3
    free_percent = 100 * usage.free / usage.total
    emit(phase="disk", target=str(target), free_gib=round(free_gib, 2), free_percent=round(free_percent, 1))
    if free_gib < minimum or free_percent < 10:
        emit(phase="disk", status="warning", detail="Low free disk space; inspect usage and request approval before any cleanup. No files were deleted.")


def acquire_lock(lock, seconds):
    started = time.monotonic()
    emit(phase="lock", status="waiting", budget_seconds=seconds)
    while True:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            remaining = seconds - (time.monotonic() - started)
            if remaining <= 0:
                raise VerificationFailure("lock", "timeout", 124, "Build directory is busy; no checks were started")
            time.sleep(min(0.05, remaining))
    emit(phase="lock", status="acquired", elapsed_seconds=round(time.monotonic() - started, 3))


def kill_group(process, signum):
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass


def run_phase(name, command, seconds, env, lock):
    started = time.monotonic()
    emit(phase=name, status="started", budget_seconds=seconds, command=command)
    process = None
    try:
        process = subprocess.Popen(command, cwd=ROOT, env=env, start_new_session=True, pass_fds=(lock.fileno(),))
        try:
            code = process.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            raise VerificationFailure(name, "timeout", 124, f"Phase exceeded {seconds:g} seconds")
        if code:
            raise VerificationFailure(name, "failed", code if code > 0 else 128 - code, f"Command exited {code}")
        emit(phase=name, status="passed", elapsed_seconds=round(time.monotonic() - started, 3))
    except VerificationFailure as error:
        error.elapsed_seconds = round(time.monotonic() - started, 3)
        raise
    except OSError as error:
        raise VerificationFailure(name, "failed", 1, str(error)) from error
    finally:
        if process is not None:
            if process.poll() is None:
                kill_group(process, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
            kill_group(process, signal.SIGKILL)
            process.wait()


def interrupt(signum, frame):
    raise VerificationFailure("interrupted", "cancelled", 128 + signum, f"Received signal {signum}")


def main():
    args = arguments()
    target = target_directory(args)
    commands = phases(args, target)
    outer_seconds = math.ceil(args.lock_seconds + sum(seconds + 2 for _, _, seconds in commands) + 60)
    emit(mode=args.mode, target=str(target), settings=SETTINGS, outer_timeout_seconds=outer_seconds,
         phases=[{"name": name, "seconds": seconds, "command": command} for name, command, seconds in commands])
    if args.plan:
        return 0
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    started = time.monotonic()
    active = "disk"
    try:
        disk_warning(target, args.min_free_gib)
        active = "lock"
        target.mkdir(parents=True, exist_ok=True)
        with (target / ".project-verify.lock").open("a+") as lock:
            acquire_lock(lock, args.lock_seconds)
            env = {**os.environ, **SETTINGS, "CARGO_TARGET_DIR": str(target)}
            for active, command, seconds in commands:
                run_phase(active, command, seconds, env, lock)
            active = "disk"
            disk_warning(target, args.min_free_gib)
        emit(mode=args.mode, status="passed", elapsed_seconds=round(time.monotonic() - started, 3))
        return 0
    except VerificationFailure as error:
        emit(phase=active if error.phase == "interrupted" else error.phase, status=error.status,
             exit_code=error.code, detail=error.detail, phase_elapsed_seconds=error.elapsed_seconds,
             elapsed_seconds=round(time.monotonic() - started, 3))
        return error.code
    except OSError as error:
        emit(phase=active, status="failed", exit_code=1, detail=str(error))
        return 1


if __name__ == "__main__":
    sys.exit(main())
