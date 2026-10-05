# Kyoto Agent verification map

This directory is the maintained source for verifying Kyoto Agent over its unix-socket `/v1` API against goldbox. Read the index before driving, then use the matching feature file as the recipe.

## Baseline preconditions

- `RUN_ID` is set. `HOME` is `/tmp/verify-kyotoagent-$RUN_ID`.
- `KYOTOAGENT_ROOT` is `$HOME/.kyotoagent`. Config, socket, and sessions live there.
- `kyotoagent serve` was started by `helpers/verify.sh launch` from a `cargo build` of this checkout.
- `helpers/verify.sh doctor` exits 0.
- Set `KYOTOAGENT_E2E_BASE_URL` in your shell environment to the model server URL ending in `/v1`.
- Never drive a serve whose socket is `~/.kyotoagent/kyotoagent.sock` on the operator account.

## Driving conventions

- Start every recipe from the baseline unless its preconditions say otherwise.
- Talk HTTP/1.1 to the unix socket with `Host: kyotoagent`, the way the TUI client does.
- Treat every command as literal.
- Auto-allow permissions with `allow_once`. Leave questions unanswered and report them.
- One user ask per session unless the feature says otherwise.

## Proof and skip reporting

- Capture the action (curl status and body) and the resulting view.
- Copy `events.jsonl` into evidence after the session is idle.
- A skipped goldbox (catalog GET failed) is a miss, not a pass.
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
- [Catalog](./catalog.md) covers goldbox `GET /models` id and advertised length.
