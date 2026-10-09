---
name: verify-project
description: Run project formatting, tests and Clippy with serialized Cargo work, consistent build settings, separate build/test deadlines, disk warnings and phase evidence. Use when verifying repository changes, running Cargo checks, or diagnosing project verification failures.
---

# Verify the project

Run from the repository root on Linux or macOS with Python 3 and the pinned Rust toolchain available.

## Plan and run

Use `python3 scripts/verify.py all --plan` to inspect commands, phase budgets, build directory and the minimum outer task deadline without starting checks or creating files.

Start `python3 scripts/verify.py all` with `start_task` and `timeout_sec: 3000`. Wait with `check_task`; retain its task ID, output and completion status. The default is offline with the lockfile enforced. When dependency downloads are authorized, add `--online`, as CI does.

The runner executes its own behavioral tests, formatting, test compilation, the full test suite including doctests, and Clippy with warnings denied. Compilation alone never establishes a test pass. Each phase prints a JSON record with its command, budget and outcome; successful phases include elapsed seconds. Cargo and test output remain visible. A failure stops immediately with the phase and exit code; a phase timeout exits 124. The runner never retries a failed check.

## Build directory and settings

Use this command for project Cargo checks instead of launching competing raw Cargo invocations. All modes take an exclusive advisory lock at `<target>/.project-verify.lock` across the complete run, including test execution. Separate build directories can proceed independently. The lock descriptor is inherited by the command so a killed supervisor cannot immediately admit another command while its child still owns the lock. Normal cancellation and phase timeouts terminate the command process group.

Target selection is `--target-dir`, then `CARGO_TARGET_DIR`, then the repository's `target` directory. Explicit relative paths resolve from the caller's working directory. Paths are canonicalized so symlink aliases use the same lock. Commands use the resolved path explicitly. Keep one build directory and the same settings for cold/warm comparisons.

Cargo's development and test profiles disable debug information and incremental compilation. The runner also sets `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, and `CARGO_INCREMENTAL=0`, matching the established CI settings. Tests and Clippy explicitly use Cargo's `test` profile. The build and test commands otherwise share the same Cargo arguments; the build phase adds `--no-run`.

## Deadlines and closeout

Default budgets in seconds:

| Phase | Budget |
| --- | ---: |
| Waiting for the directory lock | 600 |
| Verifier behavioral tests | 120 |
| Formatting | 120 |
| Test compilation | 900 |
| Test execution | 180 |
| Clippy | 900 |

Lock waiting does not consume build time; compilation does not consume test execution time. Cargo's test command can still compile doctests during the test phase. Always leave enough outer time for the sum of all selected budgets, child termination grace, and reporting. The plan prints `outer_timeout_seconds` for this purpose. Never put a long shell timeout inside a shorter tool deadline.

For focused diagnostics, use `python3 scripts/verify.py test`, `fmt`, or `clippy`. `test` also runs the verifier's behavioral tests. Budget flags are `--lock-seconds`, `--build-seconds`, `--test-seconds`, and `--check-seconds`; changing one requires recalculating the outer deadline with `--plan`.

After edits, call `get_closeout`, run each pending item with `run_closeout`, then call `get_closeout` again. The policy retains test, formatting and Clippy gates with three failed attempts per item per task. Diagnose failures from their phase and transcript before retrying. Keep the gates and retry bounds enabled; stop and report an exhausted retry budget. Manual diagnostics do not replace recorded closeout checks.

## Disk and evidence

The runner reports free disk space before and after checking. It warns below 5 GiB free or 10% free; `--min-free-gib` adjusts the absolute warning threshold. Warnings do not waive any check. The runner never deletes build outputs, removes old worktrees, or invokes `cargo clean`. Inspect disk use and ask the operator before any cleanup.

For cold evidence, select a new empty build directory when disk space permits; record its path, environment, phase timings and exit status. Repeat the same command unchanged for warm evidence. Keep logs and build outputs intact. Report test failures, lock timeouts, dependency availability and interrupted runs as observed, not as passes. Attach relevant evidence when it supports a meaningful verification claim.
