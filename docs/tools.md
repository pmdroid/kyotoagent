---
title: Tools and the gate
description: The tools a turn can call, and what stops for an answer.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/tools.md
---

`tools::Tools` is the tools a turn can call, held against one workspace.

| Tool | What it does | The limit |
| --- | --- | --- |
| `read_file` | Reads from a byte offset, or from a 1-based line | 32 KiB, and the next offset or line when there is more |
| `grep` | Searches the workspace and returns `path:line:text` | 200 matches |
| `list_dir` | One level of names, a link named as a link | 500 names |
| `search_replace` | Replaces an exact string, then takes the write path | 256 KiB |
| `write_file` | Replaces the file, through a temporary file beside it | 256 KiB |
| `generate_image` | Generates an image with a configured provider and saves it atomically | One image, 32 MiB, 300 s per HTTP request |
| `run` | Runs an argv in the workspace, with no shell | 120 s by default, 600 s at most, 64 KiB back per stream |

Every path is resolved first: relative to the workspace, then through every
symlink. A path the caller meant to be inside the workspace has to land there,
so a symlink pointing out of it is refused rather than followed, and a `..` that
walks out is the same answer. An absolute path somewhere else is a deliberate
reach, and the card names that absolute path.

## What stops at the gate

| Call | Card |
| --- | --- |
| Read, list, or grep inside the workspace | None, and nothing in the log but the model's own tool call |
| Write or search_replace | A permission card with the diff, and the turn waits |
| Read or list outside the workspace | A permission card naming the absolute path |
| Every command | A permission card with the argv, and a non-default timeout |

The answers are `allow_once`, `allow_session`, and `deny`. A `deny` is the tool
result: the workspace is as it was and the command never started. An
`allow_session` puts one exact write path, one exact outside read path, or one
exact argv on `meta.json`, so that call goes through next time without asking,
and a different path, file, or command still asks. One session's allows are that
session's; another session's log has never heard of them.

## Writes are checked against a digest

A write stores a digest of the bytes that were on disk when the card went up. If
the file changes before the answer comes back, that answer was about a file that
is no longer there, so the write drops it, asks again, and the new card carries
the diff the file has now. The `allow_session` that came with the spent answer is
spent with it.

## Diffs

A diff is kept whole up to 64 KiB, which is everything a person reads. A larger
change keeps its first 400 lines on the card, and the path and the byte size
still say how big it is. The log keeps every byte of what was proposed, so the
evidence is not lost to a short card.

## Image generation

`generate_image` sends `POST {base_url}/images/generations`. Set `provider` to a
configured provider ID to override routing. Models beginning with `grok-imagine-`
automatically use the configured `grok` provider and its saved API key or Grok
login, even when Codex is selected for coding. Models beginning with `gpt-image-`
use the configured `codex` provider and saved Codex login when available. Other
models use the active provider. Image generation keeps your coding provider unchanged. Supply `prompt`, an image `model` supported by that provider,
and an output `path`. The optional `size`, `quality`, and `response_format`
(`b64_json` or `url`) fields are forwarded when supplied. The tool requests one
image and accepts either base64 image bytes or a public image download URL.

Generation asks for permission before contacting the provider. Saving uses the
same permission and atomic-write checks as other file writes. The destination's
parent directory must already exist. Use `read_file` to inspect the saved image
and `attach_artifact` to share it.

## The turn loop

`turn::Runner` runs one ask as a tool loop against the chat client. The core tools include `read_file`, `grep`, `list_dir`, `search_replace`, `write_file`,
`run`, `get_closeout`, `run_closeout`, `ask`, `archive_session`, `finish`, and `use_skill`.

`archive_session` only archives. It hides the named session, or this session
when no id is passed. Archiving another session also stops its turn. The
directory and the log stay. The tool cannot restore or delete a session.

- A read, a search, or a list inside the workspace runs immediately.
- A write, a command, or an outside read waits on the gate, and the session reads
  `waiting` until the answer.
- `ask` appends a `question` event and blocks until the answer.
- `get_closeout` refreshes workspace changes and returns the required checks,
  their matched paths and status, and the pending IDs to run with `run_closeout`.
  It includes remaining failed attempts and any exhausted retry blocker. Query
  again after edits and before finishing because edits invalidate passed checks.
- `run_closeout` runs a check by ID, including matching setup steps, and retains
  its transcript. Review checks marked `different_model` need another model.
- `finish` appends a `result` event and ends the turn, after Stop hooks pass.

One task per running turn. An ask sent during a live turn is queued. An open question or permission
blocks new messages. A message to another session starts immediately. `cancel` stops the selected turn
at the next tool boundary: a running command gets SIGTERM and then SIGKILL two
seconds later.
