---
name: verify-kyotoagent-tui
description: Drive Kyoto Agent in an isolated real terminal, verify saved TLS server switching and remote model discovery, and capture terminal screenshots and action transcripts. Use for TUI connection, model picker, draft retention, or rendering changes.
---

# Verify Kyoto Agent TUI

Read [features/README.md](features/README.md), then the feature recipe. This skill complements the socket API checks in [verify-kyotoagent](../verify-kyotoagent/SKILL.md). It launches the real binary in a 120-column, 30-row PTY with two authenticated TLS servers and one local server. Each run has private homes, ephemeral ports, and disposable certificates.

## Launch

Run from the repository root on Linux with Python 3.11 or newer, OpenSSL, and the DejaVu Sans Mono font installed.

```sh
cargo build --bin kyotoagent
python3 -m venv /tmp/kyotoagent-tui-verify-env
/tmp/kyotoagent-tui-verify-env/bin/pip install pyte Pillow
export KYOTOAGENT_TUI_EVIDENCE="/tmp/kyotoagent-tui-proof-$(date +%s)-$$"
```

The helper creates its instances and drives them in one invocation. It waits for the Unix sockets and requires an authenticated session creation to return 201 before launching the TUI. Use a new evidence path each time. The model server must advertise at least one model through `/v1/models`.

## Doctor

```sh
/tmp/kyotoagent-tui-verify-env/bin/python .agents/skills/verify-kyotoagent-tui/helpers/drive.py --binary target/debug/kyotoagent --base-url "${KYOTOAGENT_E2E_BASE_URL:?Set KYOTOAGENT_E2E_BASE_URL in your shell environment}" --evidence "$KYOTOAGENT_TUI_EVIDENCE" --doctor
```

This read-only check runs the chosen binary's help command checks the screenshot font, and fetches the live model catalog with a five-second deadline. It prints the resolved binary, model server, and advertised IDs. Stop if it fails. `--base-url` accepts another OpenAI-compatible model server.

## Drive

```sh
/tmp/kyotoagent-tui-verify-env/bin/python .agents/skills/verify-kyotoagent-tui/helpers/drive.py --binary target/debug/kyotoagent --base-url "${KYOTOAGENT_E2E_BASE_URL:?Set KYOTOAGENT_E2E_BASE_URL in your shell environment}" --evidence "$KYOTOAGENT_TUI_EVIDENCE"
```

The helper issues and imports pairing URIs with the CLI, creates real sessions through authenticated HTTPS, and starts the ordinary TUI. It types drafts, opens Ctrl+K → Server, chooses remote A, remote B, and Local, then opens Ctrl+K → Open model. It checks session workspace identities, restored draft text, advertised model visibility, absence of unavailable picker rows, the persisted selected server, and config mode 0600. The printed JSON and `proof.json` list successful assertions.

For rendering changes, follow [rendering.md](features/rendering.md) and capture the repository's terminal screen example as well.

## Evidence

The evidence directory contains `catalog.json`, `actions.json`, `terminal.ansi`, `proof.json`, and named `.txt` and `.png` frames. PNGs render captured terminal cells in monochrome; the ANSI recording retains terminal color sequences. Screenshots must contain no pairing tokens. Inspect images before attaching them to a PR.

Proof must include the input action and resulting screen. A server switch must show the destination workspace and preserve that server's draft. Persistence proof must read the config after the UI action. Model proof compares the live catalog with the picker, including the deliberately unavailable configured model. Do not replace the actual CLI, HTTPS server, or TUI with internal setters or test endpoints.

## Cleanup

The helper's `finally` block terminates only processes it started, waits for them, closes the PTY, and removes isolated homes, keys, config, and logs. It retains screenshots, transcripts, catalog, action records, and proof. Cleanup also runs after failed assertions.

```sh
test -s "$KYOTOAGENT_TUI_EVIDENCE/proof.json"
test -s "$KYOTOAGENT_TUI_EVIDENCE/remote-models.png"
test -s "$KYOTOAGENT_TUI_EVIDENCE/terminal.ansi"
```

Never drive the operator's running TUI or remove the evidence during cleanup.

## Helpers

`helpers/drive.py` is executable and is invoked explicitly with Python in the commands above. `--doctor` checks prerequisites without creating instances. The normal invocation owns launch, drive, screenshots, assertions, and teardown.
