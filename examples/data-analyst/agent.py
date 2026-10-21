"""A private data analyst: answers questions about a sealed dataset of support
tickets from aggregates only.

The data custodian seals the dataset and the keys to the verified VM, and the
agent reads them from /run/secrets: TICKETS (the CSV), OPENROUTER_API_KEY,
API_TOKEN (what callers send as a bearer token), TLS_CERT and TLS_KEY. The
environment variable TLS_CERT_SHA256, part of the measured launch, must be the
SHA-256 of the certificate in DER form; callers check it with `vcontainer
verify` before they send anything.

POST /ask {"question": "..."} answers one question at a time, and nothing is
kept between questions. The model sees the question and what three tools
return: the schema, and counts and averages over groups of at least MIN_GROUP
tickets. The agent loads only the CATEGORIES and NUMBERS columns, so names,
emails and any other column never reach a tool. The service does not return
raw rows or include them in model prompts. Aggregates can still reveal facts
about individuals across several questions: this is not a statistical privacy
guarantee.

Environment: TLS_CERT_SHA256, MODEL (optional, an OpenRouter model id).
"""
import csv
import hashlib
import hmac
import json
import math
import os
import re
import ssl
import statistics
import sys
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = 8443
OPENROUTER = "https://openrouter.ai/api/v1/chat/completions"
MIN_GROUP = 5         # smaller groups show "<5" and no statistics
MAX_TURNS = 8         # model turns per question
MAX_QUESTION = 1000   # characters
MAX_REPLY = 1500      # tokens per model reply

# The columns the tools may group or filter by, with every value allowed in
# them. Any other value stops the agent at startup, so no free text from the
# dataset can become a group label.
CATEGORIES = {
    "product": {"Billing", "Mobile app", "Web app", "Integrations"},
    "channel": {"email", "chat", "phone"},
    "priority": {"low", "normal", "high", "urgent"},
    "team": {"Tier 1", "Tier 2", "Billing ops"},
    "status": {"open", "closed"},
}
NUMBERS = ["first_response_hours", "resolution_hours"]

SYSTEM = f"""You answer questions about a company's support tickets.

You cannot see individual tickets. Use the tools, which return only counts and
averages over groups of tickets; groups of fewer than {MIN_GROUP} tickets show
"<{MIN_GROUP}". Look at the data from the angles the question needs, then answer
in a few sentences with the numbers that support your answer. If the tools
cannot answer the question, say so."""

GROUPING = {
    "group_by": {"type": "array", "items": {"type": "string"},
                 "description": "Up to two category columns, or none for one total."},
    "where": {"type": "object", "additionalProperties": {"type": "string"},
              "description": "Only tickets whose category columns have these values."},
}
TOOLS = [
    {"type": "function", "function": {
        "name": "describe",
        "description": "The number of tickets, and the category and number columns with their values.",
        "parameters": {"type": "object", "properties": {}}}},
    {"type": "function", "function": {
        "name": "count",
        "description": "The number of tickets in each group.",
        "parameters": {"type": "object", "properties": GROUPING, "required": ["group_by"]}}},
    {"type": "function", "function": {
        "name": "average",
        "description": "The mean of a number column in each group, over the tickets that have a value.",
        "parameters": {"type": "object", "properties": dict(GROUPING, column={"type": "string", "enum": NUMBERS}),
                       "required": ["column", "group_by"]}}},
]


class ToolError(Exception):
    """A bad tool call; the message goes back to the model."""


class DataError(Exception):
    """A problem with the dataset. The message never quotes the data."""


def log(message):
    print(f"analyst: {message}", flush=True)


def load(path):
    """Reads only the category and number columns, plus the month of `created`."""
    rows = []
    with open(path, newline="") as f:
        for n, r in enumerate(csv.DictReader(f), 1):
            row = {c: r.get(c) for c in CATEGORIES}
            row["month"] = (r.get("created") or "")[:7]
            bad = [c for c, allowed in CATEGORIES.items() if row[c] not in allowed]
            if not re.fullmatch(r"\d{4}-\d{2}", row["month"]):
                bad.append("created")
            for c in NUMBERS:
                try:
                    row[c] = float(r[c]) if r.get(c) else None
                    if row[c] is not None and not (math.isfinite(row[c]) and row[c] >= 0):
                        raise ValueError
                except ValueError:
                    bad.append(c)
            if bad:
                raise DataError(f"row {n}: a value in {', '.join(bad)} is not allowed")
            rows.append(row)
    return rows


def groups(rows, args, column=None):
    """The rows that pass `where`, grouped by `group_by`."""
    allowed = dict(CATEGORIES, month={r["month"] for r in rows})
    by, where = args.get("group_by", []), args.get("where", {})
    if not isinstance(by, list) or len(by) > 2 or any(c not in allowed for c in by):
        raise ToolError(f"group_by takes up to two of {sorted(allowed)}")
    if not isinstance(where, dict) or any(c not in allowed or v not in allowed[c] for c, v in where.items()):
        raise ToolError("where takes category columns and values that describe lists")
    out = {}
    for r in rows:
        if all(r[c] == v for c, v in where.items()) and (column is None or r[column] is not None):
            out.setdefault(", ".join(f"{c}={r[c]}" for c in by) or "all", []).append(r)
    return out


def run_tool(rows, name, args):
    if name == "describe":
        cats = dict(CATEGORIES, month={r["month"] for r in rows})
        total = len(rows) if len(rows) >= MIN_GROUP else f"<{MIN_GROUP}"
        return json.dumps({"tickets": total, "categories": {c: sorted(v) for c, v in cats.items()}, "numbers": NUMBERS})
    if name == "count":
        found = groups(rows, args)
        return "\n".join(f"{g}: {len(rs) if len(rs) >= MIN_GROUP else f'<{MIN_GROUP}'}" for g, rs in sorted(found.items())) or "no tickets"
    if name == "average":
        column = args.get("column")
        if column not in NUMBERS:
            raise ToolError(f"column must be one of {NUMBERS}")
        found = groups(rows, args, column)
        return "\n".join(
            f"{g}: " + (f"{statistics.mean(r[column] for r in rs):.1f} (n={len(rs)})" if len(rs) >= MIN_GROUP else f"<{MIN_GROUP}")
            for g, rs in sorted(found.items())) or "no tickets"
    raise ToolError(f"unknown tool {name}")


def chat(messages, last):
    body = {"model": MODEL, "messages": messages, "tools": TOOLS, "max_tokens": MAX_REPLY}
    if last:
        body["tool_choice"] = "none"
    req = urllib.request.Request(OPENROUTER, json.dumps(body).encode(),
                                 {"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)["choices"][0]["message"]


def ask(question):
    """Answers one question; returns the answer and the number of tool calls."""
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": question}]
    calls = 0
    for turn in range(MAX_TURNS):
        message = chat(messages, last=turn == MAX_TURNS - 1)
        messages.append(message)
        if not message.get("tool_calls"):
            return (message.get("content") or "").strip(), calls
        for call in message["tool_calls"]:
            calls += 1
            try:
                args = json.loads(call["function"].get("arguments") or "{}")
                if not isinstance(args, dict):
                    raise ToolError("arguments must be a JSON object")
                result = run_tool(ROWS, call["function"]["name"], args)
            except (ValueError, TypeError, ToolError) as e:
                result = f"error: {e}"
            messages.append({"role": "tool", "tool_call_id": call["id"], "content": result})
    raise RuntimeError("no answer")


class Handler(BaseHTTPRequestHandler):
    timeout = 30  # also bounds the TLS handshake, which happens on the first read

    def do_POST(self):
        if self.path != "/ask":
            return self.reply(404, "not found")
        if not hmac.compare_digest(self.headers.get("Authorization", "").encode(), f"Bearer {TOKEN}".encode()):
            log("rejected a request without the right token")
            return self.reply(401, "unauthorized")
        try:
            length = int(self.headers.get("Content-Length", ""))
            if length < 0:
                raise ValueError
            question = json.loads(self.rfile.read(min(length, 4 * MAX_QUESTION)))["question"]
            if not isinstance(question, str) or not 0 < len(question) <= MAX_QUESTION:
                raise ValueError
        except (ValueError, KeyError, TypeError):
            return self.reply(400, f'send {{"question": "..."}} of up to {MAX_QUESTION} characters')
        try:
            answer, calls = ask(question)
        except Exception as e:
            # Only the type: a message could quote the question or the data.
            log(f"a question failed: {type(e).__name__}")
            return self.reply(502, "the analysis failed")
        log(f"answered a question with {calls} tool calls")
        self.reply(200, answer, "answer")

    def reply(self, code, text, key="error"):
        body = json.dumps({key: text}).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass  # request lines and client addresses stay off the console


class Server(HTTPServer):
    def handle_error(self, request, client_address):
        log(f"dropped a connection: {type(sys.exc_info()[1]).__name__}")


def secret(name):
    with open(f"/run/secrets/{name}") as f:
        return f.read().strip()


def main():
    global ROWS, KEY, TOKEN, MODEL
    cert = secret("TLS_CERT")
    if not hmac.compare_digest(hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert)).hexdigest(), os.environ.get("TLS_CERT_SHA256", "").lower()):
        sys.exit("analyst: TLS_CERT_SHA256 is not the SHA-256 of TLS_CERT; refusing to serve")
    try:
        ROWS = load("/run/secrets/TICKETS")
    except DataError as e:
        sys.exit(f"analyst: TICKETS: {e}")
    except (OSError, csv.Error) as e:
        sys.exit(f"analyst: TICKETS: {type(e).__name__}")
    KEY, TOKEN = secret("OPENROUTER_API_KEY"), secret("API_TOKEN")
    if len(TOKEN) < 32:
        sys.exit("analyst: API_TOKEN must be at least 32 characters")
    MODEL = os.environ.get("MODEL", "anthropic/claude-sonnet-5.5")
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain("/run/secrets/TLS_CERT", "/run/secrets/TLS_KEY")
    server = Server(("0.0.0.0", PORT), Handler)
    server.socket = context.wrap_socket(server.socket, server_side=True, do_handshake_on_connect=False)
    log(f"serving {len(ROWS)} tickets on :{PORT}, model {MODEL}")
    server.serve_forever()


if __name__ == "__main__":
    main()
