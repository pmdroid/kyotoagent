---
title: Slash commands
description: The commands you type into the prompt.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/slash.md
---

| Command | What it does |
| --- | --- |
| `/` | Lists matching skills |
| `/model` and `/model <id>` | Shows or switches the model |
| `/effort <level>` | Sets the reasoning effort |
| `/yolo`, `/yolo on`, `/yolo off` | Turns yolo on or off for this session. Anything else, such as `/yolo now`, is an ordinary ask |
| `/compact` | Compacts the transcript |

Archive and unarchive are command-palette rows, not slash commands. The session
list filter shows all live sessions, running sessions, sessions waiting on a
question, or archived sessions. All keeps an archived session in an `archived`
group at the bottom of the list.
