---
title: The server
description: The routes a second terminal uses to drive the agent.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/server.md
---

`kyotoagent serve` binds `~/.kyotoagent/kyotoagent.sock` with mode `0600` and serves the
sessions routes. Starting as root is refused, and a second serve exits rather
than replace a live one.

On startup every session under `~/.kyotoagent/sessions` is reloaded, so an
unanswered permission is still waiting and its answer still lands.

## Routes

| Route | What it does |
| --- | --- |
| `POST /v1/sessions` | Creates a session for an existing directory; the reply is the id, the workspace, and the status |
| `GET /v1/sessions` | Lists the sessions; a waiting row also says `permission` or `question`. Each row includes `yolo` and `allow` |
| `GET /v1/sessions/:id/view` | The quiet projection: `status`, `cards`, `revision`, and `queue` when an ask is waiting |
| `GET /v1/sessions/:id/events` | The raw log |
| `POST /v1/sessions/:id/messages` | Starts a turn: `202` and a turn id. A live turn or a compact queues a non-empty ask (`202` `{ "queued": true }`, eight at most). `409` while a permission or question is open, or when the queue is full |
| `POST /v1/sessions/:id/answers` | Answers the open card: `allow_once`, `allow_session`, `deny`, or the reply text; `409` when that card is already settled |
| `POST /v1/sessions/:id/cancel` | Stops that turn |
| `POST /v1/sessions/:id/yolo` | Sets that session's yolo flag. `true` answers an open permission with `allow_once`. `false` leaves the card waiting |

## Over HTTPS and HTTP/2

Optional `listen` in `~/.kyotoagent/config.toml` (or
`kyotoagent serve --listen 0.0.0.0:7841`, which writes that field) also serves the
same JSON API over HTTPS with HTTP/2 on that TCP address. ALPN offers `h2` and
`http/1.1`. Cert and key paths are `listen_cert` and `listen_key`, default
`~/.kyotoagent/certs/server.crt` and `server.key`. Fill those with
`tailscale cert <magicdns-name>`. Remote requests require a Bearer token with a
valid signature, expiration, issuer, and audience. `kyotoagent pair <host:port>`
prints the connection link and QR code that carry that token. The local unix
socket uses its file permissions for access.

## Restarts

On startup the runner reloads sessions from disk. An unanswered permission stays
`waiting`. A turn that died mid-flight ends with a result that the server
stopped. Allows in `meta.json` survive the restart.
