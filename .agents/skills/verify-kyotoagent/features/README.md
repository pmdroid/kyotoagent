# Kyoto Agent verification map

This directory is the maintained source for verifying Kyoto Agent over its unix-socket `/v1` API against the explicitly configured OpenAI-compatible provider. Read the index before driving, then use the matching feature file as the recipe.

## Baseline preconditions

- Start each recipe with `helpers/verify.sh run -- COMMAND ARGS...`.
- The runner supplies a fresh HOME, matching KYOTOAGENT_ROOT, socket, workspace and evidence directory to the command.
- Build the chosen binary first. The runner consumes `BIN` or this checkout's `target/debug/kyotoagent`.
- Set `KYOTOAGENT_E2E_BASE_URL`, `KYOTOAGENT_E2E_MODEL`, and the optional `KYOTOAGENT_E2E_API_KEY_ENV` before the run.
- `"$VERIFY_HELPER" doctor` exits 0 inside the run.
- Never drive the operator account's server or use its saved configuration.

## Driving conventions

- Start every recipe from the baseline unless its preconditions say otherwise.
- Talk HTTP/1.1 to the unix socket with `Host: kyotoagent`, the way the TUI client does.
- Treat every command as literal.
- The ask recipe answers permissions with `allow_once`. Capture waiting questions and let the scenario decide the answer.
- One user ask per session unless the feature says otherwise.

## Proof and skip reporting

- Capture the action (curl status and body) and the resulting view.
- Copy `events.jsonl` into evidence after the session is idle.
- A failed provider catalog request or unavailable selected model is a prerequisite failure, not a pass.
- Do not report a skipped entry point as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior. It then uses exactly four H2 sections in this order.

1. `Sub-features` lists short IDs with one line for each behavior.
2. `How to get to it (user POV)` lists every user entry point.
3. `Driving it with verify-kyotoagent` starts with `Preconditions:` and uses labeled bullets that pair each user action with an exact command and observable result.
4. `Gotchas` lists traps that can waste or invalidate a verification run.

## Features

- [Serve and ask](./serve-and-ask.md) covers creating a session, posting one ask, and reading a result card.
- [Permission](./permission.md) covers a write that waits, `allow_once`, and the file on disk.
- [Catalog](./catalog.md) covers the selected provider model and its advertised metadata.
