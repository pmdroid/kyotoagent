# Catalog

The configured OpenAI-compatible provider advertises models at `GET {base}/models`. Verify the explicitly selected model and preserve its advertised metadata.

## Sub-features

- `models-ok` returns HTTP 200 for the catalog request using the supplied credential when configured.
- `model-id` finds the exact `KYOTOAGENT_E2E_MODEL` rather than selecting the first row.
- `metadata` retains the provider's actual catalog in evidence.

## How to get to it (user POV)

- Open the TUI model picker with Ctrl+K → Open model.
- Run the verification catalog recipe against your supplied provider.

## Driving it with verify-kyotoagent

Preconditions:

- Set the provider URL, exact model, and optional credential-variable name described in the skill.

- **Launch and fetch.** Run `.agents/skills/verify-kyotoagent/helpers/verify.sh run -- bash -ec '"$VERIFY_HELPER" catalog'`.
- **Require the selected model.** The helper fails on catalog errors or an absent selected ID and prints the matching row on success.
- **Inspect evidence.** Read `models.json` in the reported evidence directory. When verifying context sizing, assert the expected advertised context field in that model's row.
- **Check doctor.** `"$VERIFY_HELPER" doctor` checks the same catalog plus server ownership and its live sessions route.

## Gotchas

- Authentication uses the named environment variable; do not paste its value into commands or evidence.
- A catalog failure is a failed prerequisite. Do not substitute another provider or the first advertised model.
- Keep real-provider verification separate from deterministic tests using a scripted local provider.
