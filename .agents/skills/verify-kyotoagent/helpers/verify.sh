#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/common.sh"
ORIG_HOME="${HOME}"

usage() {
  cat <<'EOF'
Usage: verify.sh <launch|doctor|v1|catalog|ask|permission|cleanup> [args]
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
  tmp="$(mktemp)"
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
    cp "$tmp" "$EVIDENCE/${label}.json"
    printf '%s\n' "$code" > "$EVIDENCE/${label}.status"
  fi
  printf '%s\n' "$code"
  cat "$tmp"
  rm -f "$tmp"
}

wait_socket() {
  local i
  for i in $(seq 1 80); do
    if curl --unix-socket "$SOCKET" -sS -o /dev/null --max-time 1 \
      -H 'Host: kyotoagent' "http://kyotoagent/v1/sessions"; then
      return 0
    fi
    sleep 0.1
  done
  echo "serve socket did not answer GET /v1/sessions" >&2
  return 1
}

write_config() {
  local model="$1"
  cat > "$KYOTOAGENT_ROOT/config.toml" <<EOF
base_url = "$BASE"
model = "$model"
max_steps = 8
EOF
}

first_model() {
  local body id
  body="$(curl -sS --max-time 5 "$BASE/models" || true)"
  if [ -z "$body" ]; then
    printf '%s\n' "qwen3.8-flash-next"
    return
  fi
  printf '%s\n' "$body" > "$EVIDENCE/models.json"
  id="$(python3 -c 'import json,sys
d=json.load(sys.stdin)
rows=d.get("data") or []
print(rows[0]["id"] if rows else "")
' <<<"$body" 2>/dev/null || true)"
  if [ -n "$id" ]; then
    printf '%s\n' "$id"
  else
    printf '%s\n' "qwen3.8-flash-next"
  fi
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
      fi
    fi
    sleep 1
  done
  echo "session ${sid} did not become idle" >&2
  return 1
}

require_result() {
  python3 -c '
import json,sys
view=json.load(sys.stdin)
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

do_launch() {
  BASE="$(default_base)"
  export BASE
  export RUN_ID="${RUN_ID:-$(date +%s)-$$}"
  ensure_run
  mkdirs
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "serve already running for RUN_ID=$RUN_ID pid=$(cat "$PIDFILE")" >&2
    exit 1
  fi
  HOME="$ORIG_HOME" cargo build --manifest-path "$REPO/Cargo.toml" --offline >/tmp/verify-kyotoagent-build.log 2>&1 || \
    HOME="$ORIG_HOME" cargo build --manifest-path "$REPO/Cargo.toml" >/tmp/verify-kyotoagent-build.log 2>&1
  local model
  model="$(first_model)"
  write_config "$model"
  "$BIN" serve >"$HOME/serve.log" 2>&1 &
  local pid=$!
  printf '%s\n' "$pid" > "$PIDFILE"
  if ! wait_socket; then
    kill "$pid" 2>/dev/null || true
    exit 1
  fi
  python3 -c '
import json,os,sys
meta={
  "runId": os.environ["RUN_ID"],
  "home": os.environ["HOME"],
  "kyotoagentRoot": os.environ["KYOTOAGENT_ROOT"],
  "socket": os.environ["SOCKET"],
  "pid": int(open(os.environ["PIDFILE"]).read().strip()),
  "bin": os.environ["BIN"],
  "baseUrl": os.environ["BASE"],
  "model": sys.argv[1],
  "evidence": os.environ["EVIDENCE"],
  "workspace": os.environ["WORKSPACE"],
}
text=json.dumps(meta)+"\n"
open(os.environ["INSTANCE"],"w").write(text)
open(os.path.join(os.environ["EVIDENCE"],"instance.json"),"w").write(text)
print(json.dumps(meta))
' "$model"
}

do_doctor() {
  BASE="$(default_base)"
  export BASE
  ensure_run
  mkdirs
  local code exe expected pid
  code="$(curl -sS -o "$EVIDENCE/models.json" -w '%{http_code}' --max-time 5 "$BASE/models" || true)"
  if [ "${code:-000}" != "200" ]; then
    echo "goldbox GET $BASE/models did not answer 200 (got ${code:-000})" >&2
    exit 1
  fi
  if [ ! -S "$SOCKET" ]; then
    echo "serve socket missing: $SOCKET" >&2
    exit 1
  fi
  code="$(curl --unix-socket "$SOCKET" -sS -o "$EVIDENCE/sessions.json" -w '%{http_code}' \
    --max-time 5 -H 'Host: kyotoagent' "http://kyotoagent/v1/sessions" || true)"
  if [ "${code:-000}" != "200" ]; then
    echo "serve GET /v1/sessions did not answer 200 (got ${code:-000})" >&2
    exit 1
  fi
  if [ ! -f "$PIDFILE" ]; then
    echo "no pidfile at $PIDFILE" >&2
    exit 1
  fi
  pid="$(cat "$PIDFILE")"
  if [ ! -d "/proc/$pid" ]; then
    echo "serve pid $pid is not running" >&2
    exit 1
  fi
  exe="$(readlink -f "/proc/$pid/exe" | sed 's/ (deleted)$//')"
  expected="$(readlink -f "$BIN")"
  if [ "$exe" != "$expected" ]; then
    echo "serve binary is $exe, expected $expected" >&2
    exit 1
  fi
  echo "doctor ok pid=$pid model=$(python3 -c 'import json,sys; print((json.load(sys.stdin).get("data") or [{}])[0].get("id",""))' < "$EVIDENCE/models.json")"
}

do_catalog() {
  BASE="$(default_base)"
  export BASE
  ensure_run
  mkdirs
  local code
  code="$(curl -sS -o "$EVIDENCE/models.json" -w '%{http_code}' --max-time 5 "$BASE/models" || true)"
  if [ "${code:-000}" != "200" ]; then
    echo "goldbox GET $BASE/models did not answer 200 (got ${code:-000})" >&2
    exit 1
  fi
  python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
rows=d.get("data") or []
if not rows:
    sys.stderr.write("catalog has no rows\n")
    sys.exit(1)
row=rows[0]
ident=row.get("id") or ""
if not ident:
    sys.stderr.write("first row has no id\n")
    sys.exit(1)
length=row.get("context_length") or row.get("max_model_len") or row.get("context_window") or row.get("max_input_tokens")
if not length:
    sys.stderr.write("first row has no advertised length\n")
    sys.exit(1)
print(f"{ident} length={length}")
' "$EVIDENCE/models.json"
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
  local created sid code view
  created="$(curl --unix-socket "$SOCKET" -sS --max-time 10 \
    -H 'Host: kyotoagent' -H 'Content-Type: application/json' \
    -X POST --data "$(python3 -c 'import json,os; print(json.dumps({"workspace":os.environ["WORKSPACE"]}))')" \
    "http://kyotoagent/v1/sessions")"
  printf '%s\n' "$created" > "$EVIDENCE/session.json"
  sid="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$created")"
  code="$(curl --unix-socket "$SOCKET" -sS -o "$EVIDENCE/message.json" -w '%{http_code}' \
    --max-time 10 \
    -H 'Host: kyotoagent' -H 'Content-Type: application/json' \
    -X POST --data "$(python3 -c 'import json,sys; print(json.dumps({"text":sys.argv[1]}))' "$text")" \
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
  python3 -c '
import sys
text=open(sys.argv[1]).read()
if "pong" not in text.lower():
    sys.stderr.write("ping.txt missing pong: %r\n" % text)
    sys.exit(1)
print("ping.txt ok")
' "$WORKSPACE/ping.txt"
}

do_cleanup() {
  ensure_run
  local pid
  if [ -f "$PIDFILE" ]; then
    pid="$(cat "$PIDFILE")"
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      sleep 2
      if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || true
      fi
    fi
    rm -f "$PIDFILE"
  fi
  if [ -d "$KYOTOAGENT_ROOT" ]; then
    rm -rf "$KYOTOAGENT_ROOT"
  fi
  echo "cleaned serve for RUN_ID=$RUN_ID; evidence at $EVIDENCE"
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
  launch) do_launch "$@" ;;
  doctor) do_doctor "$@" ;;
  catalog) do_catalog "$@" ;;
  ask) do_ask "$@" ;;
  permission) do_permission "$@" ;;
  cleanup) do_cleanup "$@" ;;
  v1) do_v1 "$@" ;;
  *) usage; exit 2 ;;
esac
