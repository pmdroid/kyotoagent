---
title: Getting started
description: Install Kyoto Agent, run kyotoagent serve, and start your first session.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/getting-started.md
---

Kyoto Agent is a small coding agent that runs on a remote machine. The binary
and Rust crate are named `kyotoagent`.

## Install

Install the stable Rust toolchain with Cargo, then clone and build:

```sh
git clone https://github.com/pmdroid/kyotoagent.git
cd kyotoagent
cargo install --path .
```

That puts `kyotoagent` on your `PATH`. Run `cargo test` first if you want to check
the build.

`scripts/install-serve.sh` installs `kyotoagent serve` as a user service, so the
socket stays up after you close the terminal. On Linux that is a systemd user
service. On macOS it is a LaunchAgent. `scripts/install-serve.sh --uninstall`
stops the service and removes the unit.

## Connect a model

For subscription sign-in or API-key entry, follow [Two terminals](#two-terminals)
below, then open **Providers** in Kyoto Agent.

Alternatively, configure an OpenAI-compatible provider before starting the server:

```sh
kyotoagent provider add local --base-url http://127.0.0.1:11434/v1 --model <model-id>
```

For a provider that requires a key, add `--api-key-env MODEL_API_KEY` and export
that variable in the terminal that runs `kyotoagent serve`. See [model configuration](model.md)
for provider and credential options.

## Two terminals

`kyotoagent serve` runs the agent on the machine that holds your code. A second
terminal, or a phone-sized window, drives it.

Terminal one:

```sh
kyotoagent serve
```

Terminal two, on the same machine:

```sh
cd /path/to/your/project
kyotoagent
```

In the Kyoto terminal, press Ctrl-K and choose **Providers** to connect a model.
For a subscription, select Grok or Codex and approve the device code at the URL
shown. For an API key, select a configured provider and enter its key. To add a provider,
choose **Add OpenAI-compatible provider** and enter its server URL, model, and key.
Credentials are saved on the selected server.

Bare `kyotoagent` opens the newest session for the current directory; if there is
none, the session list stays empty until you press Ctrl-T to start one.

## Where things live

- Config: `~/.kyotoagent/config.toml`
- Socket: `~/.kyotoagent/kyotoagent.sock`
- Sessions: `~/.kyotoagent/sessions`
- Closeout: `<workspace>/.kyotoagent/closeout.yaml`, or `<workspace>/.agents/closeout.yaml` when that Kyoto file is absent
- Skills: `.kyotoagent/skills`, then `.agents/skills`
- Hooks: `.agents`

## Reaching it from another device

Set `listen` in `~/.kyotoagent/config.toml`, or pass it on the command line:

```sh
kyotoagent serve --listen 0.0.0.0:7841
```

`scripts/install-serve.sh --listen 0.0.0.0:7841` installs the user service with that listen address.

Serve then also speaks the same JSON API over HTTPS with HTTP/2 on that TCP
address. Fill `listen_cert` and `listen_key` (default
`~/.kyotoagent/certs/server.crt` and `server.key`) with, for example,
`tailscale cert <magicdns-name>`. Remote clients authenticate with a signed
connection link from the server. Generate the link and QR code with:

```sh
kyotoagent pair box.tailnet.ts.net:7841
```

Pass the generated link through `--url` or `KYOTOAGENT_URL`, or scan its QR
code on the mobile connect screen:

```sh
kyotoagent --url 'kyotoagent://box.tailnet.ts.net:7841?token=<generated-token>' sessions
```

## Next

- [CLI](cli.md), every subcommand.
- [The model](model.md) to configure a provider.
- [The server](server.md), the routes a second terminal uses.
