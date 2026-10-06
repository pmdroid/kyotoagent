---
title: CLI
description: Every kyotoagent subcommand, and how each one reaches the server.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/cli.md
---

The subcommands talk to the server over its socket, so a second terminal can
drive the agent while `kyotoagent serve` stays up. Every command follows the same
choice of socket or `--url` / `KYOTOAGENT_URL`.

## Commands

| Command | What it does |
| --- | --- |
| `kyotoagent` | Opens the newest session for the current directory |
| `kyotoagent --yolo` | Opens it with yolo on |
| `kyotoagent attach <id>` | Opens the list with that session selected |
| `kyotoagent attach <id> --yolo` | The same, with yolo on |
| `kyotoagent serve` | Starts the server |
| `kyotoagent serve --listen <addr>` | Also serves HTTPS + HTTP/2 on that TCP address, and writes `listen` to config |
| `kyotoagent new [repo-id]` | Creates a session in a registered repository, or offers repository selection in a terminal |
| `kyotoagent repos` | Lists registered repository ids, names, and paths |
| `kyotoagent pair <host:port>` | Prints a temporary pairing link and QR code, valid for 10 minutes |
| `kyotoagent sessions` | Lists the sessions, one plain-text row each |
| `kyotoagent log [id]` | Prints the event log of a session |
| `kyotoagent cancel [id]` | Cancels the current turn of a session |
| `kyotoagent provider` | Prints the current model server |
| `kyotoagent provider use <id>` | Selects a table in the file |
| `kyotoagent provider add <id> --base-url <url> --model <model> [--api-key-env <name>]` | Inserts a table and selects it |
| `kyotoagent doctor` | Checks all configured credentials, model catalogs, the socket, and the closeout file. Print the system prompt with `systemprompt` |
| `kyotoagent systemprompt` | Prints the system prompt for the current directory |
| `kyotoagent doctor --url <connection-link>` | Checks remote server connectivity alongside local configuration and credentials |

`doctor` checks authentication for every configured provider and queries each model
catalog. It also checks the selected model and verifies configured Exa and
Firecrawl keys through their account endpoints. Failed checks name the provider
or environment variable and return a nonzero exit status. Credential values are
never printed.

## Reaching the server

- `kyotoagent --url <connection-link>` or `KYOTOAGENT_URL` uses HTTPS with HTTP/2
  and the signed token from `kyotoagent pair`.
- Bare `kyotoagent` still uses the unix socket.

`new`, `repos`, `sessions`, `log`, `cancel`, and `attach` follow the same choice.

`kyotoagent` with no arguments opens the newest session for the current directory.
`kyotoagent log` and `kyotoagent cancel` name the newest session in the current
directory when no id is given. If the socket is down, the program tells you to
start `kyotoagent serve`.

```sh
kyotoagent serve --listen 0.0.0.0:7841
kyotoagent
kyotoagent --url 'kyotoagent://box.tailnet.ts.net:7841?token=<generated-token>' sessions
kyotoagent attach <id>
```
