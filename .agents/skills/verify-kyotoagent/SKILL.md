---
name: verify-kyotoagent
description: Test Kyoto Agent through its real /v1 routes in a disposable instance with an explicitly supplied OpenAI-compatible provider and model.
---

# Verify Kyoto Agent

Read [features/README.md](features/README.md), then the relevant recipe. Use the ordinary binary and `/v1` routes for observed behavior.

## Launch

Build the checkout with `cargo build --bin kyotoagent`, then run:

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh run
```

Set these shell variables before running:

- `KYOTOAGENT_E2E_BASE_URL`: your OpenAI-compatible provider root ending in `/v1`.
- `KYOTOAGENT_E2E_MODEL`: the exact model ID.
- `KYOTOAGENT_E2E_API_KEY_ENV`: the name of the environment variable holding the provider key, when authentication is required.
- `BIN`: optional path to the chosen build; defaults to this checkout's `target/debug/kyotoagent`.
- `KYOTOAGENT_E2E_TIMEOUT`: optional total run deadline in seconds; defaults to 240.

The default run creates a session, asks for `pong`, and checks that the result came from a model reply or successful `finish`. Use `run --text "..."` for another prompt.

The foreground runner creates a fresh `/tmp/verify-kyotoagent-*` home, config, workspace and socket. It sets HOME and KYOTOAGENT_ROOT together, configures the supplied model for chat and titles, enables HTTPS on `127.0.0.1:0`, and discovers the actual address through `/v1/https`. It prints instance metadata after readiness. Never read/write the operator's Kyoto configuration or stop the installed server.

## Doctor

The runner validates the selected provider model after startup. Inside a custom driver, `"$VERIFY_HELPER" doctor` also checks server process ownership and the sessions route. Stop on failure.

## Drive

Use `-- COMMAND ARGS...` to run a verification command inside the temporary workspace. The runner exports `VERIFY_HELPER`, `SOCKET`, `VERIFY_ADDRESS`, `WORKSPACE`, and `EVIDENCE` along with the private HOME/root.

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh run -- bash -ec '"$VERIFY_HELPER" doctor; "$VERIFY_HELPER" permission'
```

A custom driver can call `"$VERIFY_HELPER" v1 METHOD PATH [BODY] [--label NAME]`, `catalog`, `ask [--text TEXT]`, or `permission`. `doctor` checks process ownership, the provider's selected model, and the live run's sessions route. Stop on failure.

`ask` posts one session and message, answers permission events with `allow_once`, and waits for a model result. A waiting question is captured and reported for the scenario driver to handle by its event ID. Drive one mapped feature per run unless a change touches more.

Use `start_task` for the foreground runner, then collect its completion through `check_task`.

## Evidence

The final line names the retained evidence directory. It contains `instance.json`, `outcome.json`, `serve.log`, `driver.log`, the provider catalog, and the action/view/event files produced by the recipe. The supplied credential is redacted from retained text. Inspect evidence before publishing it.

## Cleanup

On completion, startup failure, timeout, or interruption, the runner verifies server ownership, stops its process groups, waits for the processes, and removes temporary config/workspace/certificates. Evidence remains. Exit 124 means timeout; driver failures preserve their nonzero status.

## Helpers

`helpers/verify.sh run` owns the lifecycle. Its recipe commands reuse `common.sh` to validate the temporary instance. `helpers/test_run.py` exercises the runner with the real binary and a local scripted provider through `cargo test --test verify_helper` on Linux.

Capture both the action and its resulting view. For writes, read back the file and retain the readback. A scripted provider establishes deterministic harness behavior; a real-provider run establishes the live-model behavior. A failed prerequisite or model error is not a passing verification.
