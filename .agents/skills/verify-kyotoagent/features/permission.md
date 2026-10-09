# Permission

A write inside the workspace opens a permission card. The turn continues only after `allow_once` (or `allow_session`). The file is absent before the answer and present after.

## Sub-features

- `permission-open` appends a permission event and sets the session `waiting`.
- `allow-once` posts `allow_once` for that event id, the TUI yolo path.
- `write-lands` creates the file in the workspace after the allow.
- `quiet-view` keeps the tool name out of the cards.

## How to get to it (user POV)

- Ask the agent to create or replace a file, then press `a` or enable yolo (`--yolo` / Ctrl-Y).
- POST `/v1/sessions/:id/answers` with `{ "id": "<permission-event-id>", "choice": "allow_once" }`.

## Driving it with verify-kyotoagent

Preconditions:

- Inside `helpers/verify.sh run -- COMMAND ARGS...`, `"$VERIFY_HELPER" doctor` exits 0.
- The workspace has no `ping.txt`.

- **Ask for a write.** Run `"$VERIFY_HELPER" permission`. The helper asks `Create ping.txt containing the single word pong.`
- **See the card.** Poll until `status` is `waiting` and `GET /v1/sessions` has `waiting: permission`, or until idle if the model finished without a write. If a permission never appears, record the idle view and stop; that is a miss for this feature.
- **Allow once.** POST `allow_once` for the open permission event id. Expect 204. Save `permission-answer.json`.
- **Confirm the file.** When idle, `ping.txt` in the workspace contains `pong` (case-insensitive is enough). `view-idle.json` has a result card. `events.jsonl` still names `write_file`. The card JSON does not.

## Gotchas

- Answer the event id from `events.jsonl`, not the view card id (`c1`).
- A second answer to the same card is 409. First answer wins.
- Yolo on the TUI posts `allow_once` for permissions and still waits on questions. This helper does the same.
- The model may read before it writes. Reads inside the workspace leave no card.
