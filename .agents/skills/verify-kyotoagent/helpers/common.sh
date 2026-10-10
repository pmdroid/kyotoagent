#!/usr/bin/env bash
set -euo pipefail

ensure_run() {
  python3 -B -c 'import sys; sys.path.insert(0, sys.argv[1]); from run import context; context()' "$SCRIPT_DIR"
  export EVIDENCE="$HOME/evidence"
  export WORKSPACE="$HOME/workspace"
  export SOCKET="$KYOTOAGENT_ROOT/kyotoagent.sock"
}
