# Saved server connections

Pairing remembers a server, and the TUI switches between saved servers and Local while preserving each server's session and draft.

## Sub-features

- `servers.pair`: a code expires after 10 minutes or one exchange; the client saves the returned JWT in private config.
- `servers.switch`: the selected server changes the visible workspace.
- `servers.draft`: returning to a server restores its unsent text.
- `servers.local`: Local uses the Unix socket.
- `servers.persist`: the final selected server survives in config.

## How to get to it (user POV)

Use `kyotoagent pair HOST:PORT` to issue a URI and `kyotoagent pair URI` to import it. In the TUI, open Ctrl+K → Server or enter `/server` in the active input.

## Driving it with PTY

Preconditions: Launch and Doctor succeed.

- Run the normal helper command in SKILL.md. It invokes both pairing CLI paths, then uses Ctrl+K → Server and numbered choices.
- Inspect `server-picker-*.png` for the saved hosts and Local, `restored-a.png` and `restored-b.png` for their matching workspaces and drafts, and `local.png` for the local view.
- Require `proof.json` to report authenticated switching, Local, draft restoration, persisted selection, and private config. Config is read before its isolated home is removed.
- When changing slash commands, additionally type `/server` in the active input and capture the resulting picker and choice. Record that entry point separately.

## Gotchas

- Ports and server names change per run. The helper derives numbered choices from the displayed order.
- Pairing tokens stay in temporary private config and are removed during cleanup.
- Switching to an unreachable saved server must leave the previous draft and connection visible; exercise this path when changing failure handling.
