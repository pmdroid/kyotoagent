---
title: Screenshots
description: Render the demo frames to PNG with a real terminal emulator.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/screenshots.md
---

```sh
pip install pyte
python3 scripts/capture-frames.py
```

This runs the example under a pty, waits for each frame to settle, and replays
the output through a terminal emulator into `build/screens`: one PNG per frame
plus a page with all three. It compares the captured text with the golden files
first, so a screenshot never shows a screen the tests would reject.

`--check` runs the comparison without writing any image.
