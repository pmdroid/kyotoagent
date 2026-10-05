---
title: The model
description: Configure an OpenAI-compatible provider in ~/.kyotoagent/config.toml.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/model.md
---

`~/.kyotoagent/config.toml` names the current provider and a table per provider.
Each table is an OpenAI-compatible server: a `base_url`, a `model`, and an
optional `api_key_env`.

```toml
provider = "office"
title_model = "google/gemini-3.8-flash"
yolo = false

[providers.office]
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
api_key_env = "OPENROUTER_API_KEY"

[providers.local]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder"

[projects.kyotoagent]
path = "/work/kyotoagent"
yolo = true
```

`title_model` is the model that names a new session. When omitted or set to
`google/gemini-3.8-flash`, it follows the provider: Grok uses `grok-4.6` with
low reasoning effort, Codex uses `gpt-6-luna`, OpenCode uses
`deepseek-v4.1-flash`, and OpenRouter uses `google/gemini-3.8-flash`.
Set another model ID to override that choice. An empty string leaves the
directory name. Title requests keep the session's model and effort unchanged.

`base_url` is the OpenAI-compatible root. The client appends
`/chat/completions`, so the file never spells that path out. The `local` table
above is Ollama's root. A llama.cpp server is `http://127.0.0.1:8080/v1`. Omit
`api_key_env` when the server has no auth.

## Keys

`api_key_env` is the *name* of an environment variable, never the key itself,
so the secret stays in the environment and the file stays safe to read.

- When the name is set and the variable holds something, the request carries
  `Authorization: Bearer`.
- When it is left out, or the variable is unset or empty, no `Authorization`
  header goes out at all, so a local server is not asked for a key it does not
  want.

## Subscription providers

Start `kyotoagent serve`, open `kyotoagent`, then press Ctrl-K and choose
**Providers**. Select Grok or Codex and approve the device code at the URL shown.
The selected server saves the credentials and uses that provider for new turns.

## Managing providers

- `kyotoagent provider` prints the current id, its `base_url`, and its `model`.
- `kyotoagent provider use <id>` selects a table already in the file.
- `kyotoagent provider add <id> --base-url <url> --model <model> [--api-key-env <name>]`
  inserts a table and selects it.

Changing `provider` is picked up on the next turn; serve does not need to
restart.

`kyotoagent provider add opencode` asks for an API key and writes `opencode-auth.json`
next to `config.toml`, mode 0600. The table is kind `opencode`, Zen at
`opencode.ai/zen/v1`, and model `kimi-k2.7-code`. `--model` replaces that model.
`--api-key-env` stores the variable name and skips the key file. The key is not
written into `config.toml`.

A file that still has top-level `base_url` and `model` loads as it did. That is
the current provider when `provider` and `[providers]` are absent.

## Projects and approval defaults

A `[projects.<id>]` table names a folder a new session can start in. `path` is
required. `name` is optional and falls back to the id. The id is lowercase
letters, digits, and dashes, the same rule as a provider id. Ctrl-T asks serve
for that list. With no projects, Ctrl-T still asks this directory or a git
worktree of the current directory.

`yolo` on the file is the default for a new session. A project can set its own
`yolo`. When that key is present, the new session uses it. When the key is
absent, the session uses the file default. Leave both out and the session
starts off. A later edit to the file leaves sessions that already exist as
they are. Ctrl-Y, `/yolo`, and `POST /v1/sessions/:id/yolo` change the live
session. `true` answers an open permission once. `false` leaves that card
waiting and does not forget paths already allowed for the session.


## Project closeout fallback

Set a project's `closeout` to a YAML file path to use it when the workspace has
neither `.kyotoagent/closeout.yaml` nor `.agents/closeout.yaml`. A repository
closeout file takes precedence, including a file with an empty items list.
Relative configuration paths start at the configured project directory. For a
fallback outside the project, imports and skills start beside its YAML file.

```toml
[projects.kyotoagent]
path = "/work/kyotoagent"
closeout = "/shared/closeout.yaml"
```

Closeout 0.1 imports resolve depth-first and prefix check IDs with their `as`
namespace, such as `quality/tests`. Nested import paths start at the policy
root.

The app and TUI label each check as required or not required for the current
turn. A check becomes required when the turn touches a workspace path matching
its `paths`. Omitting `paths` makes any workspace write require that check.
Read-only turns show "Not required; won't run" and finish without checks.
Required checks must pass before Kyoto finishes the turn or runs `gh pr create`.
Hiding the closeout pane changes its visibility while checks remain required.
Further workspace writes invalidate their passes.

Matching setup steps run in order before the check. A failed setup stops the
remaining steps and the check. Commands use their `exec` arguments and
`timeoutSeconds` directly. The app and TUI show output as the command runs;
each new attempt starts with an empty log.

## The request

Chat Completions providers receive streaming requests at `/chat/completions`.
Kyoto assembles text and tool calls from the stream before executing tools.
Servers that return a single JSON completion are also supported. Providers
using the Responses API follow their configured protocol.

A transport failure or an unsuccessful reply reaches the turn loop as an error.
The session log records the result of the turn.
