---
title: TUI
description: The keys that drive the live screen.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/tui.md
---

The live screen is quiet on purpose. It shows asks, questions, permissions,
results, and proof, and spends its space on looking deliberate.

## Keys

| Key | Action |
| --- | --- |
| Ctrl-C | Detaches |
| Ctrl-N / Ctrl-P | Move the session list |
| Ctrl-T | Lists the folders from config, then asks this directory or a git worktree |
| Ctrl-X | Stops the turn |
| Ctrl-Y | Toggles yolo |
| Ctrl-M | Opens the model picker |
| Ctrl-K | Opens the command palette |
| right-click a session | Archive, unarchive, or delete it |
| `?` | On an empty prompt, opens help |
| Enter or a click | Opens the overlay |
| Esc | Closes a popup |
| `a` / `s` / `d` | Answers a permission |
| `1`-`9` | Picks a listed question choice while the prompt is empty |

Typing in the prompt is the ask, or a free-text question answer.

## Providers

Press Ctrl-K and choose **Providers** to authenticate on the selected server.
Select Grok or Codex to start device-code sign-in. Select an API-key provider
to enter its key. The server saves credentials; remote clients do not store them.

## Queueing

Typing during a turn queues the next ask. Enter sends it. The footer shows
`queued N`.

## Projects

A `[projects.<id>]` table names a folder a new session can start in. `path` is
required, `name` is optional and falls back to the id, and the id is lowercase
letters, digits, and dashes. Optional `yolo` on that table is the mode a new
session in that folder starts in. When the key is absent, the session uses the
top-level `yolo` in `config.toml`, which is off when omitted. Ctrl-T asks the
server for that list. With no projects, Ctrl-T still asks this directory or a
git worktree of the current directory.
