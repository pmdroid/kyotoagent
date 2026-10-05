---
title: Hooks
description: Command hooks that run before and after a write or a command, and when finish would stop.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/hooks.md
---

Command hooks load from these files, later entries append:

1. `~/.agents/hooks.json`
2. every `~/.agents/hooks/*.json`
3. `<workspace>/.agents/hooks.json`
4. `<workspace>/.agents/hooks/*.json`

A missing directory is an empty source. `$HOME` in a command string expands.

`PreToolUse` and `PostToolUse` run for `write_file` and `run`. A matcher of
`write_file` also matches `Write`, `Edit`, `MultiEdit`, `write`, and
`search_replace`. A matcher of `run` also matches `Bash` and
`run_terminal_command`. Reads, `list_dir`, `ask`, and `use_skill` skip
`PreToolUse`. Hooks write no cards.

## Contract

- A `PreToolUse` command receives `{ "tool_name", "tool_input" }` on stdin.
  Exit 2 denies the call: stderr is the tool result and the write or command
  does not run. A crash or a 30-second timeout denies `write_file` and `run`.
- A `PostToolUse` command receives the same plus `tool_result`. Exit 2 is
  feedback the model reads on the next step; the tool already ran.
- `Stop` runs when the model calls `finish`. Exit 2 keeps the session working,
  stderr is a tool result, and there is no proof event yet. A crash or timeout
  on Stop lets finish through.

## Example

A sample `~/.agents/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "run",
        "hooks": [
          {
            "type": "command",
            "command": "python3 $HOME/.agents/hooks/pre_tool_review_guard.py"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "write_file",
        "hooks": [
          {
            "type": "command",
            "command": "python3 $HOME/.agents/hooks/no_code_comments.py --hook"
          }
        ]
      },
      {
        "matcher": "run",
        "hooks": [
          {
            "type": "command",
            "command": "python3 $HOME/.agents/hooks/pr_size_warn.py"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 $HOME/.agents/hooks/stop_closeout_guard.py"
          }
        ]
      }
    ]
  }
}
```
