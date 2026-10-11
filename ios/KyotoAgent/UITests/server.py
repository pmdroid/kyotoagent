import argparse
import json
import ssl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--cert", required=True)
parser.add_argument("--key", required=True)
parser.add_argument("--port-file", required=True)
args = parser.parse_args()
fixtures = Path(__file__).resolve().parents[2] / "Fixtures"
session = json.loads((fixtures / "sessions.json").read_text())[1]
session.update(status="idle", waiting=None, enhance=False, title="Composer UI test")
view = {"status": "idle", "cards": [], "revision": 1, "skills": [{"name": "preflight", "description": "Ship checks", "user_invocable": True, "disable_model_invocation": False, "path": "preflight"}]}

class Handler(BaseHTTPRequestHandler):
    def reply(self, status, value):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        if status != 204:
            self.wfile.write(json.dumps(value).encode())

    def do_GET(self):
        if self.headers.get("Authorization") != "Bearer composer-ui-test":
            self.reply(401, {})
        elif self.path == "/v1/sessions":
            self.reply(200, [session])
        elif self.path.endswith("/view"):
            self.reply(200, view)
        elif self.path == "/v1/models":
            self.reply(200, [{"id": "fixture-model", "reasoning_efforts": []}])
        else:
            self.reply(200, [])

    def do_POST(self):
        if self.headers.get("Authorization") != "Bearer composer-ui-test":
            self.reply(401, {})
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path == "/v1/pair":
            self.reply(200, {"server": {"name": "Composer fixture", "version": "1", "workspace": "/test", "model": "fixture-model", "yolo": False, "enhance": False, "show_closeout": False}, "client": body, "models": [], "repositories": []})
            return
        if self.path.endswith("/messages") and body.get("text") == "/fixture closeout":
            session.update(status="waiting", waiting="question")
            view.update(status="waiting", closeout_bypassed=False, closeout=[{"id": "tests", "kind": "command", "hint": "Run tests", "status": "failed", "required": True, "attempt": 3, "exit": 1, "tail": "Test suite failed"}])
            view["revision"] += 1
            view["cards"] = [{"id": str(view["revision"]), "kind": "question", "at": "now", "body": {"eventId": "closeout-question", "text": "Check tests failed 3 times. Retry limit reached.\nStop to investigate, or accept failed closeout for this session. Acceptance skips further closeout checks and lets this session finish. Failures stay recorded.", "choices": ["Stop", "Accept failed closeout for this session"], "answer": None}}]
            self.reply(202, {"turnId": "fixture"})
            return
        if self.path.endswith("/answers"):
            if body.get("id") != "closeout-question" or body.get("choice") != "Accept failed closeout for this session":
                self.reply(400, {})
                return
            session.update(status="idle", waiting=None)
            view.update(status="idle", closeout_bypassed=True)
            view["revision"] += 1
            view["cards"][0]["body"]["answer"] = body["choice"]
            self.reply(204, {})
            return
        if self.path.endswith("/model/session"):
            session["model"] = body["model"]
            text = "Model saved"
        elif self.path.endswith("/messages"):
            text = body["text"]
            if text == "/closeout enable":
                view["closeout_bypassed"] = False
                text = "Closeout enabled for this session with a fresh retry budget."
        else:
            self.reply(204, {})
            return
        view["revision"] += 1
        view["cards"] = [{"id": str(view["revision"]), "kind": "ask", "at": "now", "body": {"text": text}}]
        if self.path.endswith("/model/session"):
            self.reply(204, {})
        else:
            self.reply(202, {} if body.get("text") == "/closeout enable" else {"turnId": "fixture"})

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(args.cert, args.key)
server.socket = context.wrap_socket(server.socket, server_side=True)
Path(args.port_file).write_text(str(server.server_port))
server.serve_forever()
