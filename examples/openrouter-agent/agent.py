"""A tiny agent: POST /chat {"message": "..."} answers through OpenRouter.

The API key is never in the image or the launch config: vcontainer seals it
to the verified VM and mounts it at /run/secrets/OPENROUTER_API_KEY.
"""
import json
import os
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

KEY = open("/run/secrets/OPENROUTER_API_KEY").read().strip()
MODEL = os.environ.get("MODEL", "openai/gpt-4o-mini")


def chat(message):
    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=json.dumps({"model": MODEL, "messages": [{"role": "user", "content": message}]}).encode(),
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)["choices"][0]["message"]["content"]


class Handler(BaseHTTPRequestHandler):
    def reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.reply(200, {"agent": "vcontainer demo", "model": MODEL})

    def do_POST(self):
        if self.path != "/chat":
            return self.reply(404, {"error": "not found"})
        length = int(self.headers.get("Content-Length", 0))
        message = json.loads(self.rfile.read(length) or b"{}").get("message", "")
        try:
            self.reply(200, {"model": MODEL, "reply": chat(message)})
        except Exception as e:  # report upstream errors to the caller
            self.reply(502, {"error": str(e)})


print(f"agent listening on :8080 (model {MODEL})", flush=True)
ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
