---
name: verify-kyotoagent
description: Drive Kyoto Agent over unix-socket /v1 against the live goldbox OpenAI-compatible server, with an isolated HOME and KYOTOAGENT_ROOT. Use when proving serve, catalog, a permissioned ask, or when running /verify-kyotoagent.
---

# Verify kyotoagent

Kyoto Agent speaks HTTP on a unix socket. This skill launches `kyotoagent serve` under `/tmp/verify-kyotoagent-$RUN_ID`, talks to it the way the TUI does (`Host: kyotoagent` over that socket), and points the model at goldbox. Read [features/README.md](features/README.md) before driving. Drive one mapped feature per run unless the change touches more.

Never set `HOME` to the operator's home. Never read or write `~/.kyotoagent` on the real account.

## Launch

```sh
cd "$(git rev-parse --show-toplevel)"
export RUN_ID="${RUN_ID:-$(date +%s)-$$}"
.agents/skills/verify-kyotoagent/helpers/verify.sh launch
```

`launch` builds `target/debug/kyotoagent` with the operator HOME so rustc and mise stay put, then creates `/tmp/verify-kyotoagent-$RUN_ID`, sets `HOME` to that directory and `KYOTOAGENT_ROOT=$HOME/.kyotoagent`, writes `config.toml` for goldbox using the required `KYOTOAGENT_E2E_BASE_URL` shell environment variable, starts `$BIN serve`, and waits until `GET /v1/sessions` on the socket returns 200. stdout is one JSON object (also saved as `$HOME/instance.json`). Ready means that file exists and `doctor` exits 0.

Teardown is `helpers/verify.sh cleanup`.

## Doctor

Run first whenever anything looks off.

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh doctor
```

Checks, in order:

1. `GET $BASE/models` returns 200 within 5 seconds. On any other outcome it prints `goldbox GET $BASE/models did not answer 200` and exits 1.
2. `GET /v1/sessions` on the serve socket returns 200.
3. `/proc/$pid/exe` is the `target/debug/kyotoagent` this run built.

A down goldbox fails step 1 and stops. Do not drive after a failed doctor.

## Drive

Harness is curl over the unix socket, same routes the TUI uses. `helpers/verify.sh v1 METHOD PATH [BODY]` is the wrapper. Stable handles are the `/v1` paths, JSON fields `id`, `status`, `waiting`, `cards[].kind`, `cards[].body.text`, and permission event ids in `events.jsonl`.

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh v1 GET /v1/sessions
.agents/skills/verify-kyotoagent/helpers/verify.sh catalog
.agents/skills/verify-kyotoagent/helpers/verify.sh ask --text "Reply with the single word pong."
.agents/skills/verify-kyotoagent/helpers/verify.sh permission
```

`ask` is POST `/v1/sessions` then POST `/v1/sessions/:id/messages`. While the view is `waiting` on a permission it posts `allow_once` (the TUI yolo path). It does not answer questions. When the session is idle it requires a result card with non-empty `body.text`. One turn. No compact. No second ask.

Feature recipes live under [features/](features/).

## Evidence

`/tmp/verify-kyotoagent-$RUN_ID/evidence/` keeps the proof. Cleanup leaves this directory.

| file | source |
| --- | --- |
| `instance.json` | launch metadata (pid, socket, binary, base URL, model) |
| `models.json` | goldbox `GET /models` body |
| `sessions.json` | socket `GET /v1/sessions` |
| `session.json` | `POST /v1/sessions` body |
| `message.json` | `POST .../messages` status and body |
| `view-idle.json` | `GET .../view` once idle |
| `events.jsonl` | copy of the session log |
| `permission-answer.json` | each `allow_once` reply |
| `workspace-ls.txt` | workspace listing after a write |

Proof standards:

- Drive `/v1` the way the TUI does. Do not call `ChatClient` or write session files by hand as the action.
- Capture the request and the resulting view, not only the idle screen.
- For a write, read the file back from the workspace.
- Goldbox is a real model server. A canned fake in `tests/` is a different path.

## Cleanup

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh cleanup
```

Sends SIGTERM, then SIGKILL after 2 seconds, to the pid recorded at launch. Removes `$HOME/.kyotoagent` scratch besides the evidence directory. Leaves `/tmp/verify-kyotoagent-$RUN_ID/evidence/`. Never `pkill kyotoagent`. Run cleanup after every attempt, failed included.

Confirm the evidence files are still under `/tmp/verify-kyotoagent-$RUN_ID/evidence/` before you report.

## Helpers

```sh
.agents/skills/verify-kyotoagent/helpers/verify.sh launch
.agents/skills/verify-kyotoagent/helpers/verify.sh doctor
.agents/skills/verify-kyotoagent/helpers/verify.sh v1 METHOD PATH [BODY] [--label NAME]
.agents/skills/verify-kyotoagent/helpers/verify.sh catalog
.agents/skills/verify-kyotoagent/helpers/verify.sh ask [--text TEXT]
.agents/skills/verify-kyotoagent/helpers/verify.sh permission
.agents/skills/verify-kyotoagent/helpers/verify.sh cleanup
```

`helpers/common.sh` is sourced by `verify.sh`. It owns `RUN_ID`, `HOME`, `KYOTOAGENT_ROOT`, `BASE`, and the socket path.
