---
title: Building and tests
description: Build Kyoto Agent, run the suite, and refresh the screen goldens.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/development.md
---

## Build and test

```sh
cargo test
```

## Screen goldens

`tests/screens/*.txt` are the three demo frames, drawn at 76 by 24 through
ratatui's `TestBackend` and compared by `cargo test`:

| File | The state |
| --- | --- |
| `waiting.txt` | A session blocked on a write permission, with its diff and the answer keys |
| `idle.txt` | A finished turn, with the allowed permission, the question, the result, and the proof |
| `working.txt` | A session still working, showing only its ask |
| `closeout-run.txt` | A closeout check waiting on permission, naming the command the harness built |
| `closeout-asked.txt` | A check that used all its failed attempts, so the harness asks instead |

After a deliberate layout change, redraw the frames and read the diff:

```sh
UPDATE_GOLDENS=1 cargo test --test screens
```

## Watching it

```sh
cargo run --example screens
```

Enter moves to the next frame, `q` leaves. The terminal wants to be 76 columns
by 24 rows, the size the frames were drawn at.

The chat client can be watched the same way. This starts a loopback HTTP server,
points the real `ChatClient` at it through a real `Config`, and prints the bytes
that crossed the wire:

```sh
cargo run --example chat
cargo run --example chat -- --with-key
```

The two runs differ by one line, and that line is the point: a local model is
not sent a key, and OpenRouter is.

## This docs site

See [working on the website](website.md) for the Astro build, content layout, and deployment.
