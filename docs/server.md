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
| `POST /v1/sessions/:id/archive` | `{ "archived": true }` stops the turn, hides the session, and removes its subagent sessions. `{ "archived": false }` restores it. The directory stays. A message to an archived session is `409` |

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

## Direct APNs notifications

Push is optional. Add this table to the server's `config.toml` and restart `kyotoagent serve`:

```toml
[push]
team_id = "YOUR_APPLE_TEAM_ID"
key_id = "YOUR_APNS_KEY_ID"
private_key = "keys/AuthKey_YOUR_APNS_KEY_ID.p8"
topic = "sh.pascal.kyotoagent"
```

The topic defaults to `sh.pascal.kyotoagent`. Relative key paths resolve under the server root. Use an Apple APNs P-256 PKCS8 `.p8` key, keep it outside source control, and restrict its permissions to 0600. Invalid push configuration prevents server startup. The server does not print key material, device tokens, authorization headers, or APNs response bodies.

An authenticated client registers with `PUT /v1/devices`:

```json
{"id":"12345678-1234-1234-1234-123456789abc","serverId":"abcdef12-1234-1234-1234-123456789abc","token":"hex-encoded-apns-device-token","environment":"sandbox"}
```

Use `production` for production tokens. The client supplies its stable installation ID and the routing ID of the saved server connection. Register again after token rotation or reconnecting. Both IDs must be UUIDs. Tokens accept an even number of hexadecimal characters, up to 1024 characters, rather than assuming a fixed APNs token length. Registration replaces the entry for that installation. `DELETE /v1/devices` takes `{"id":"12345678-1234-1234-1234-123456789abc"}`. Both operations return 204 and use the existing HTTPS bearer-token middleware. The owner-only Unix socket can also manage registrations. Registration is allowed without `[push]`, but delivery stays disabled.

The registry is `devices.json` under the server root, atomically replaced with mode 0600. Back it up as sensitive data. APNs 410 responses remove only the registration matching the token and environment used for that request, so a rotated token survives an older request's response.

While serving, a separate observer polls persisted sessions once a second. It sends alerts for new result events and currently unanswered permission or question events. Event IDs prevent duplicate delivery within that serving lifetime. Startup history is marked seen without sending it. YOLO permissions, answered requests, and requests no longer in a waiting session do not generate alerts. The delivery worker rechecks requests before each attempt.

Alerts show the session title (or ID), the reason for the alert, and a preview of the actual question, permission action, or result text. These previews are sent through APNs and can appear on the lock screen according to iOS notification preview settings. Titles are capped at 100 characters and body previews at 400, with whitespace collapsed and an ellipsis for longer content. Outside the required `aps` object, payloads contain only `serverId`, `sessionId`, `eventId`, and `kind`. Clients fetch the current session after opening an alert. The iOS app uses native notification presentation rather than custom in-app session banners.

Delivery uses HTTP/2 and cached ES256 provider tokens signed with ring. Sandbox and production registrations use Apple's corresponding endpoints. Requests set the configured topic, `apns-push-type: alert`, priority 10, and expiration 0. The queue holds 64 events; a full queue drops new notifications rather than delaying a turn. Transport errors, HTTP 429, and HTTP 5xx receive at most three attempts with one- and two-second delays. Each request times out after ten seconds. Notifications are best effort, not a durable delivery log. There is no production endpoint override; the local fake APNs endpoint exists only in Rust tests.
