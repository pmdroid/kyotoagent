# Remote model picker

The model picker shows models advertised by the selected server's configured providers.

## Sub-features

- `models.remote`: a remote server supplies its catalog.
- `models.available`: an unavailable configured model is absent.
- `models.credentials`: OpenRouter without a configured key contributes no rows.
- `models.select`: choosing a row updates the session model.

## How to get to it (user POV)

Open Ctrl+K → Open model or press Ctrl+M in a terminal that distinguishes it from Enter. Select the desired model row.

## Driving it with PTY

Preconditions: Launch and Doctor succeed with a nonempty live model catalog.

- Run the normal helper command in SKILL.md. It opens the picker while connected to remote A.
- Compare `catalog.json` and `remote-models.png`. Require an advertised ID to appear and `unavailable-configured-model` to be absent.
- The remote server also names an OpenRouter provider without credentials. Require `unconfigured-router` to be absent from picker rows and `proof.json` to report unconfigured provider exclusion.
- When changing selection, choose a displayed row and capture the updated session header, then read `GET /v1/sessions/:id/view` through that same authenticated server to confirm its model.

## Gotchas

- Ctrl+M can arrive as Enter; the command palette is the reliable automated entry point.
- A model catalog can change between runs. Derive IDs from the catalog rather than hard-coding a model.
- Multiple providers can advertise the same ID. Verify the chosen provider when changing selection logic.
