#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/common.sh"

usage() {
  cat <<'EOF'
Usage: verify.sh run [--text TEXT | -- COMMAND ARGS...]
Inside run: verify.sh <doctor|v1|catalog|ask|permission> [args]
EOF
}

cmd="${1:-}"
if [ -z "$cmd" ]; then
  usage
  exit 2
fi
shift || true

v1() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local label="${4:-}"
  local out tmp code
  tmp="$(mktemp "$HOME/request-XXXXXX")"
  if [ -n "$body" ]; then
    code="$(curl --unix-socket "$SOCKET" -sS -o "$tmp" -w '%{http_code}' \
      --max-time 30 \
      -H 'Host: kyotoagent' \
      -H 'Content-Type: application/json' \
      -X "$method" \
      --data "$body" \
      "http://kyotoagent${path}")"
  else
    code="$(curl --unix-socket "$SOCKET" -sS -o "$tmp" -w '%{http_code}' \
      --max-time 30 \
      -H 'Host: kyotoagent' \
      -X "$method" \
      "http://kyotoagent${path}")"
  fi
  if [ -n "$label" ]; then
    [[ "$label" =~ ^[a-zA-Z0-9_-]+$ ]] || { echo "invalid evidence label" >&2; return 2; }
    cp "$tmp" "$EVIDENCE/${label}.json"
    printf '%s\n' "$code" > "$EVIDENCE/${label}.status"
  fi
  printf '%s\n' "$code"
  cat "$tmp"
  rm -f "$tmp"
}

open_permission_id() {
  local sid="$1"
  python3 -c '
import json,sys
open_id=None
for line in sys.stdin:
    line=line.strip()
    if not line:
        continue
    ev=json.loads(line)
    kind=ev.get("kind")
    if kind=="permission":
        open_id=ev.get("id")
    elif kind=="permission_answer":
        open_id=None
print(open_id or "")
' <<<"$(curl --unix-socket "$SOCKET" -sS --max-time 10 -H 'Host: kyotoagent' "http://kyotoagent/v1/sessions/${sid}/events")"
}

wait_idle() {
  local sid="$1"
  local i status view perm code body
  for i in $(seq 1 240); do
    view="$(curl --unix-socket "$SOCKET" -sS --max-time 10 -H 'Host: kyotoagent' "http://kyotoagent/v1/sessions/${sid}/view")"
    status="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("status") or "")' <<<"$view")"
    if [ "$status" = "idle" ]; then
      printf '%s\n' "$view" > "$EVIDENCE/view-idle.json"
      curl --unix-socket "$SOCKET" -sS --max-time 10 -H 'Host: kyotoagent' \
        "http://kyotoagent/v1/sessions/${sid}/events" > "$EVIDENCE/events.jsonl"
      printf '%s\n' "$view"
      return 0
    fi
    if [ "$status" = "waiting" ]; then
      perm="$(open_permission_id "$sid")"
      if [ -n "$perm" ]; then
        body="$(python3 -c 'import json,sys; print(json.dumps({"id":sys.argv[1],"choice":"allow_once"}))' "$perm")"
        code="$(curl --unix-socket "$SOCKET" -sS -o "$EVIDENCE/permission-answer.json" -w '%{http_code}' \
          --max-time 10 \
          -H 'Host: kyotoagent' -H 'Content-Type: application/json' \
          -X POST --data "$body" \
          "http://kyotoagent/v1/sessions/${sid}/answers")"
        printf '%s\n' "$code" > "$EVIDENCE/permission-answer.status"
        [ "$code" = 204 ] || { echo "permission answer returned $code" >&2; return 1; }
      else
        printf '%s\n' "$view" > "$EVIDENCE/view-waiting.json"
        echo "session is waiting for a question answer" >&2
        return 1
      fi
    fi
    sleep 1
  done
  echo "session ${sid} did not become idle" >&2
  return 1
}

require_result() {
  python3 -c '
import json,os,sys
view=json.load(sys.stdin)
events=[json.loads(line) for line in open(os.path.join(os.environ["EVIDENCE"], "events.jsonl"))]
results=[event for event in events if event.get("kind")=="result"]
if not results:
    sys.exit("no result event")
turn=results[-1].get("turnId")
turn_events=[event for event in events if event.get("turnId")==turn]
model_texts=[event["body"].get("text") for event in turn_events if event.get("kind")=="model_message"]
finished=any(event.get("kind")=="tool_result" and event["body"].get("tool")=="finish" and not event["body"].get("is_error") and event["body"].get("output")==results[-1]["body"].get("text") for event in turn_events)
if not finished and results[-1]["body"].get("text") not in model_texts:
    sys.exit("turn ended without a model result")
cards=view.get("cards") or []
result=next((c for c in cards if c.get("kind")=="result"), None)
if result is None:
    sys.stderr.write("no result card\n")
    sys.exit(1)
text=((result.get("body") or {}).get("text") or "").strip()
if not text:
    sys.stderr.write("result text is empty\n")
    sys.exit(1)
print(text)
'
}

do_doctor() {
  ensure_run
  python3 "$SCRIPT_DIR/run.py" doctor
}

do_catalog() {
  do_doctor
}

do_ask() {
  ensure_run
  local text="Reply with the single word pong."
  while [ $# -gt 0 ]; do
    case "$1" in
      --text) text="$2"; shift 2 ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done
  local created sid code view request
  created="$(curl --unix-socket "$SOCKET" -sS --max-time 10 \
    -H 'Host: kyotoagent' -H 'Content-Type: application/json' \
    -X POST --data "$(python3 -c 'import json,os; print(json.dumps({"workspace":os.environ["WORKSPACE"]}))')" \
    "http://kyotoagent/v1/sessions")"
  printf '%s\n' "$created" > "$EVIDENCE/session.json"
  sid="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$created")"
  request="$(python3 -c 'import json,sys; print(json.dumps({"text":sys.argv[1]}))' "$text")"
  printf '%s\n' "$request" > "$EVIDENCE/request.json"
  code="$(curl --unix-socket "$SOCKET" -sS -o "$EVIDENCE/message.json" -w '%{http_code}' \
    --max-time 10 \
    -H 'Host: kyotoagent' -H 'Content-Type: application/json' \
    -X POST --data "$request" \
    "http://kyotoagent/v1/sessions/${sid}/messages")"
  printf '%s\n' "$code" > "$EVIDENCE/message.status"
  if [ "$code" != "202" ]; then
    echo "POST /messages answered $code" >&2
    exit 1
  fi
  view="$(wait_idle "$sid")"
  require_result <<<"$view" >/dev/null
  echo "session $sid idle with a result"
}

do_permission() {
  ensure_run
  rm -f "$WORKSPACE/ping.txt"
  do_ask --text "Create ping.txt containing the single word pong. Then finish."
  if [ ! -f "$WORKSPACE/ping.txt" ]; then
    echo "ping.txt was not written" >&2
    ls -la "$WORKSPACE" > "$EVIDENCE/workspace-ls.txt" || true
    exit 1
  fi
  ls -la "$WORKSPACE" > "$EVIDENCE/workspace-ls.txt"
  cp "$WORKSPACE/ping.txt" "$EVIDENCE/ping.txt"
  python3 -c '
import sys
text=open(sys.argv[1]).read()
if "pong" not in text.lower():
    sys.stderr.write("ping.txt missing pong: %r\n" % text)
    sys.exit(1)
print("ping.txt ok")
' "$WORKSPACE/ping.txt"
}

do_v1() {
  ensure_run
  local method="${1:-}"
  local path="${2:-}"
  local body=""
  local label=""
  shift 2 || true
  while [ $# -gt 0 ]; do
    case "$1" in
      --label) label="$2"; shift 2 ;;
      *) body="$1"; shift ;;
    esac
  done
  if [ -z "$method" ] || [ -z "$path" ]; then
    echo "v1 METHOD PATH [BODY] [--label NAME]" >&2
    exit 2
  fi
  v1 "$method" "$path" "$body" "$label"
}

case "$cmd" in
  run) exec /usr/bin/python3 "$SCRIPT_DIR/run.py" "$@" ;;
  doctor) do_doctor "$@" ;;
  catalog) do_catalog "$@" ;;
  ask) do_ask "$@" ;;
  permission) do_permission "$@" ;;
  v1) do_v1 "$@" ;;
  *) usage; exit 2 ;;
esac
