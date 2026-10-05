---
title: Internals
description: Source modules, prompt construction, and the execution of a turn.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/internals.md
---

## Source map

This build contains the screen, the session log, the permission-gated tools, the
model client, the turn loop, and the server that ties them together:

- `src/screen.rs` holds the screen model, plain data, and the one `render`
  function that draws it.
- `src/mock.rs` holds the three hardcoded states, so the tests and the example
  cannot drift apart.
- `src/main.rs` is the `kyotoagent` binary: the live TUI, `attach`, `serve`, `new`,
  `sessions`, `log`, `cancel`, `provider`, and `doctor`.
- `src/doctor.rs` is `kyotoagent doctor`: the socket, the selected model catalog,
  and the workspace closeout file.
- `src/auth.rs` handles provider authentication and stores credentials under
  `~/.kyotoagent`.
- `src/tui.rs` is the live screen: it polls the socket or `--url` / `KYOTOAGENT_URL`,
  maps the quiet cards, and draws them through `screen::render`.
- `src/session.rs` is one session directory: `meta.json` beside an append-only
  `events.jsonl`.
- `src/events.rs` is what a line of that log is made of.
- `src/view.rs` projects the log to the cards the screen draws.
- `src/tools.rs` is `read_file`, `grep`, `list_dir`, `search_replace`, `write_file`,
  and `run` against one workspace.
- `src/permit.rs` is the gate they stop at, and what an `allow_session` answer
  remembers.
- `src/config.rs` is `~/.kyotoagent/config.toml`: named providers and the one
  serve uses.
- `src/chat.rs` sends model requests and assembles text and tool calls from
  streaming or JSON replies.
- `src/prompt.rs` builds the short system prompt from the workspace, the tool
  rules, and the skill index.
- `src/turn.rs` runs one ask as a tool loop against the chat client, with one
  task per running turn and a child process that is killed when its turn is
  cancelled.
- `src/hooks.rs` loads command hooks from `~/.agents` and the workspace. The
  turn loop runs them before a write or a command, after it, and when `finish`
  would stop.
- `src/server.rs` speaks HTTP on a unix socket under `~/.kyotoagent`, and the same
  JSON API over HTTPS with HTTP/2 when `listen` is set.


## The turn loop

`turn::Runner` runs one ask as a tool loop against the chat client. An ask
appends `user_ask`, sets the session `working`, and posts the model transcript.
The model sees every tool call and tool result. The session view stays the
projection from the log: a read inside the workspace leaves no card, and a
write or a command waits on the permission gate.

The core tools include `read_file`, `grep`, `list_dir`, `search_replace`,
`write_file`, `run`, `ask`, `finish`, and `use_skill`. A read, a search, or a
list inside the workspace runs immediately. A write, a command, or an outside
read waits on the gate, and the session reads `waiting` until the answer. `ask`
appends a `question` event and
blocks until the answer. `finish` appends a `result` event and ends the turn,
after Stop hooks have passed.

One task per running turn. A message sent during a live turn is queued. An open question or permission
blocks new messages. A message to another session starts immediately. Tasks share no lock, so a
session waiting on a permission does not block its siblings. `cancel` stops the
selected turn at the next tool boundary: a running command gets SIGTERM and
then SIGKILL two seconds later, and the turn ends with a result that says it
was stopped.

A model response with text and no tool call becomes the result. A
failed model request ends the turn with a one-sentence result.

On startup, the runner reloads sessions from disk. An unanswered permission
stays `waiting`. A turn that died mid-flight ends with a result that the
server stopped. Allows in `meta.json` survive the restart.

`prompt::system_prompt` builds the short system prompt from the workspace
path, the tool rules, the skill index (name and description only), and a
line telling the model to call `ask` when a decision is missing and
`finish` with the result. Skill bodies stay out of the prompt.

Kyoto Agent reads workspace instructions from `AGENTS.md`.
The loader reads `~/.kyotoagent/AGENTS.md`, then `~/.agents/AGENTS.md`.
It then walks from the git root to the workspace. In each directory it reads
`AGENTS.md`, `.kyotoagent/AGENTS.md`, and `.agents/AGENTS.md` in that order.
In workspace instruction folders, a missing `AGENTS.md` permits `agents.md`
instead. An empty file or a symlink at the uppercase path prevents that fallback.
Without a git root, it reads the workspace directory. Empty files are skipped.
A symlink is followed when its target is a regular file inside the same root:
the project for a workspace file, and home for `~/.kyotoagent/AGENTS.md` and
`~/.agents/AGENTS.md`. A link that leaves that root is skipped.

When the combined text exceeds 32 KiB, the prompt keeps its first 32 KiB
and final 8 KiB, separated by `AGENTS.md truncated.`. Cuts preserve UTF-8
character boundaries.
