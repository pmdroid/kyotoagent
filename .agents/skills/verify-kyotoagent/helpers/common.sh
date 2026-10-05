#!/usr/bin/env bash
set -euo pipefail

skill_dir() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}

repo_root() {
  git -C "$(skill_dir)" rev-parse --show-toplevel
}

default_base() {
  printf '%s\n' "${KYOTOAGENT_E2E_BASE_URL:?Set KYOTOAGENT_E2E_BASE_URL to an OpenAI-compatible server URL ending in /v1}"
}

ensure_run() {
  if [ -z "${RUN_ID:-}" ]; then
    if [ -n "${VERIFY_KYOTOAGENT_HOME:-}" ]; then
      RUN_ID="${VERIFY_KYOTOAGENT_HOME#/tmp/verify-kyotoagent-}"
    elif [ -f "${HOME:-}/instance.json" ] && [ "${HOME:-}" != "${HOME_REAL:-}" ]; then
      RUN_ID="${HOME#/tmp/verify-kyotoagent-}"
    else
      echo "set RUN_ID (or run launch first)" >&2
      exit 1
    fi
  fi
  export RUN_ID
  export VERIFY_KYOTOAGENT_HOME="/tmp/verify-kyotoagent-${RUN_ID}"
  export HOME="$VERIFY_KYOTOAGENT_HOME"
  export KYOTOAGENT_ROOT="$HOME/.kyotoagent"
  export EVIDENCE="$HOME/evidence"
  export WORKSPACE="$HOME/workspace"
  export SOCKET="$KYOTOAGENT_ROOT/kyotoagent.sock"
  export INSTANCE="$HOME/instance.json"
  export PIDFILE="$HOME/serve.pid"
  export REPO="$(repo_root)"
  export BIN="${BIN:-${CARGO_TARGET_DIR:-$REPO/target}/debug/kyotoagent}"
}

mkdirs() {
  mkdir -p "$KYOTOAGENT_ROOT" "$EVIDENCE" "$WORKSPACE"
}

save_label() {
  local label="$1"
  local dest="$EVIDENCE/${label}"
  cat > "$dest"
  printf '%s\n' "$dest"
}
