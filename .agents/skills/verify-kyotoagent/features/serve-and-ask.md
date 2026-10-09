# Serve and ask

A user starts `kyotoagent serve`, opens a session for a workspace, types one ask, and later sees a result card with the model's answer.

## Sub-features

- `serve-up` binds the unix socket and answers `GET /v1/sessions`.
- `session-create` creates a session for an existing workspace directory.
- `ask-once` starts a turn with `POST /v1/sessions/:id/messages`.
- `result-card` shows a result with non-empty text once the session is idle.

## How to get to it (user POV)

- Run `kyotoagent serve` in a terminal, then `kyotoagent` to open the TUI, then type an ask.
- From another process, `POST /v1/sessions` and `POST /v1/sessions/:id/messages` on the socket.

## Driving it with verify-kyotoagent

Preconditions:

- Inside `helpers/verify.sh run -- COMMAND ARGS...`, `"$VERIFY_HELPER" doctor` exits 0.
- No session exists yet in this `KYOTOAGENT_ROOT`.

- **Create a session.** Point serve at the run workspace. Run `"$VERIFY_HELPER" ask --text "Reply with the single word pong."` The helper POSTs `/v1/sessions` with that workspace and expects 201 and an `id`.
- **Post one ask.** The same command POSTs `/v1/sessions/:id/messages` with that text and expects 202.
- **Wait for idle.** Poll `GET /v1/sessions/:id/view`. If `status` is `waiting` and a permission is open, POST `allow_once`. Do not post a second ask. Do not POST `/compact`.
- **Read the result.** When `status` is `idle`, `cards` includes `kind: result` and `body.text` is non-empty and matches a model reply or successful finish tool result. Save `view-idle.json` and `events.jsonl` under `evidence/`.

## Gotchas

- Serve sends the real tool list to the configured provider. Verify the agent through its session routes.
- A live complete can take up to 120 seconds. Wait on idle, not a fixed sleep.
- `409` on messages means the session is still working or compacting. Poll the view.
- The result card is the proof. A `200` on messages is the wrong status; create is 201, ask is 202.
