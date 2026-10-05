# Terminal progress and pane rendering

The empty pane is clear. Active work uses a braille spinner and action labels, Waiting stays stationary, and YOLO uses a stable amber color.

## Sub-features

- `rendering.empty`: the empty session pane has no dog splash.
- `rendering.action`: the selected header and footer show the active action.
- `rendering.waiting`: question and permission gates show Waiting.
- `rendering.yolo`: enabled YOLO stays amber across animation frames.
- `rendering.motion`: slow refreshes leave typing and animation responsive.

## How to get to it (user POV)

Start the TUI with an empty session, send a prompt, observe model and tool work, and toggle YOLO with Ctrl+Y. Questions and permissions open during the turn.

## Driving it with PTY

Preconditions: the build and Doctor succeed. Install Chromium for PNG capture by the existing screen helper.

- Run `python3 scripts/capture-frames.py --out "$KYOTOAGENT_TUI_EVIDENCE/rendering"`. It drives the real terminal screen example and compares captured cells with checked-in golden screens before exporting PNGs.
- Inspect `working.png` for Reading in the header and footer, `waiting.png` for Waiting, and `todos.png` for the sidebar layout. Retain the generated HTML and PNG files.
- The live helper's `local.png` proves the empty pane in the ordinary TUI. Inspect it for the absence of splash graphics.
- For responsiveness changes, delay the selected server's GET responses through a TLS proxy, type an unsent draft during a pending refresh, and retain timestamped ANSI output. Compare input echo time and spinner frame intervals while the request is outstanding. Then switch Remote → Local → Remote and confirm the draft returns.

## Gotchas

- A screenshot cannot prove animation cadence; preserve timestamped frames for motion checks.
- The example uses fixture content. Capture an ordinary live TUI as well when changing data refresh or action dispatch.
- Keep the current text input intact when checking progress rendering.
