# Catalog

Goldbox advertises models at `GET {base}/models`. The first row has an id and an advertised context length. Kyoto Agent uses that length for compaction and the model picker.

## Sub-features

- `models-ok` returns HTTP 200 for `GET {base}/models`.
- `model-id` exposes a non-empty `id` on the first row.
- `advertised-length` exposes `context_length`, `max_model_len`, `context_window`, or `max_input_tokens`.

## How to get to it (user POV)

- Open the TUI model picker with Ctrl-M or `/model`.
- `curl $KYOTOAGENT_E2E_BASE_URL/models` (goldbox, no key).

## Driving it with verify-kyotoagent

Preconditions:

- Launch has written `instance.json`. Goldbox may be checked before serve is up.

- **Fetch the catalog.** Run `.agents/skills/verify-kyotoagent/helpers/verify.sh catalog`. That is `GET $BASE/models` with a 5 second timeout.
- **Require 200.** Any other status or a transport error prints `goldbox GET $BASE/models did not answer 200` and exits 1. Save the body as `evidence/models.json`.
- **Read the first row.** `data[0].id` is non-empty. An advertised length is present (`max_model_len` on goldbox today).
- **Doctor uses the same GET.** A down goldbox fails doctor and stops the run.

## Gotchas

- Goldbox needs no `Authorization` header. Do not send one.
- `cargo test --offline` must never call this host. Live rust coverage is `KYOTOAGENT_E2E=1 cargo test --test e2e -- --ignored`.
- Off the tailnet, the GET fails fast. That is a skip for the ignored rust tests and a doctor failure for this skill.
