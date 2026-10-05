---
title: The session log
description: One directory per session, one JSON object per line per turn, and the cards the view draws.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/sessions.md
---

A session is a directory. A turn is one JSON object per line in
`events.jsonl`:

```json
{ "id": "e1", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1", "kind": "user_ask", "body": { "text": "..." } }
```

`meta.json` holds what does not change event by event: the id, the workspace,
the model, the timestamps, the status, and the exact paths and argv one
`allow_session` answer remembered.

## Events to cards

`view::read` turns a session directory into `{ status, cards, revision }`, where
`revision` is the number of events and `status` is the meta's. Five kinds of
event open a card, and three draw nothing:

| Event | Card |
| --- | --- |
| `user_ask` | `ask`, with the text |
| `question` | `question`, with the text, the choices, and the answer once it is in |
| `permission` | `permission`, with the action, the path, the diff or the argv, and the decision |
| `result` | `result`, with the text |
| `proof` | `proof`, with what was written, the diffstat, and the checks that failed |
| `model_message`, `tool_call`, `tool_result` | none, and no tool name anywhere in the cards |

That last row is the whole point. The log holds every tool call the model made,
so a read is in the transcript and its return is too, and neither reaches the
screen.

## An answer changes a card

An answer changes a card rather than adding one. Once a write is allowed, the
card reads `Allowed replace of README.md` and the diff and the argv are gone
from the view, because the user is no longer being asked. The log keeps both,
so the evidence survives a reload:

```json
{ "id": "c2", "kind": "permission", "at": "2026-09-29T00:00:00.000Z",
  "body": { "action": "Replace README.md", "path": "/w/README.md",
            "timeoutSec": null, "decision": "Allowed replace of README.md" } }
```

## Proof

Once a turn is done, its proof card lists every closeout item that ran, in the
order they ran, with the kind the file named and the outcome of each:

```text
▎ PROOF
│ Wrote README.md
│ M README.md
│ README.md | 1 +
│ ✓ test  command  passed
│ ✗ lint  command  failed
```

A turn that ran no items lists none rather than hiding the list, so the
difference is visible.
