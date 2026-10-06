---
title: Skill evals
description: Compare automatic skill selection and task behavior across Git revisions on a live model server.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/skill-evals.md
---

Run the same requests against Git revisions using a live OpenAI-compatible server:

```sh
export KYOTOAGENT_E2E_BASE_URL="http://your-model-server:8888/v1"
python3 evals/skills.py \
  --variant baseline=main \
  --variant matching=fix/automatic-skill-matching \
  --variant work=feat/adapt-agent-work-instructions \
  --repeats 2 --jobs 3
```

The runner builds each revision in a temporary worktree and starts an isolated Kyoto server. Each case gets a fresh workspace and session through `/v1`. Git, Cargo, Python 3, curl, and cached Cargo dependencies are required. Builds use `CARGO_TARGET_DIR` when set, or the current repository's `target` directory.

`--model` selects a model from the server's catalog. The default is its first model. `--cases` selects a case file. `--output` selects a new artifact directory; the default is a timestamped directory under `/tmp`. `--timeout` bounds each turn in seconds. Variant order rotates across cases and repetitions.

Cases cover implicit and explicit matching, verification after editing, unrelated requests, and a matching skill with model invocation disabled. Each case declares which skills are expected and when they must load. A work skill must load successfully before workspace tools run. A verification skill must load before the verification command.

Scoring uses recorded tool events and an independent check of the resulting Python behavior. An extra skill invocation fails the selection score. The verifier, skill files, README, and user notes must retain their original contents. Read-only cases also preserve the application file. Verification credit requires an actual successful tool result from running the fixture checks.

`summary.md` compares skill selection, task outcomes, verification, and full passes by revision. `scores.json` records each case and repetition. The manifest records commit hashes, model, concurrency, timeout, and the eval runner digest. Each run retains the system prompt, request, event log, final view, file diff, independent check output, and score. Builds and server logs are retained too.

Run the scorer checks with:

```sh
python3 -m unittest discover -s evals -p 'test_*.py'
```
