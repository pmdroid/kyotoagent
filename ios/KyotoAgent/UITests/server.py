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
        if self.path.endswith("/model/session"):
            session["model"] = body["model"]
            text = "Model saved"
        elif self.path.endswith("/messages"):
            text = body["text"]
        else:
            self.reply(204, {})
            return
        view["revision"] += 1
        view["cards"] = [{"id": str(view["revision"]), "kind": "ask", "at": "now", "body": {"text": text}}]
        self.reply(202, {"turnId": "fixture"})

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(args.cert, args.key)
server.socket = context.wrap_socket(server.socket, server_side=True)
Path(args.port_file).write_text(str(server.server_port))
server.serve_forever()
