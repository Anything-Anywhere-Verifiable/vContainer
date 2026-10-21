"""Ask the private data analyst a question, after verifying it.

    python3 ask.py --policy analyst-policy.json --token api-token.key IP [QUESTION]

First runs `vcontainer verify` against the custodian's policy and reads the
expected certificate fingerprint, TLS_CERT_SHA256, from the launch that the
policy pins. Then, for each question, it connects and compares the server's
certificate with that fingerprint before sending the token. Without QUESTION
it asks for questions until an empty line or `quit`.
"""
import argparse
import base64
import hashlib
import http.client
import json
import ssl
import subprocess
import sys

PORT = 8443


def expected_fingerprint(policy_path):
    """TLS_CERT_SHA256 and the image from the launch the policy pins. The
    launch payload is issued_at (8 bytes), SHA-512 of the secrets (64 bytes)
    and the launch config as JSON."""
    with open(policy_path) as f:
        launch = json.load(f).get("launch")
    if not launch:
        sys.exit("ask: the policy has no launch; export it after the deploy")
    config = json.loads(base64.b64decode(launch)[72:])
    # Exactly once, bare names included: with a repeated variable the agent
    # would see the last value, and a bare name sets it to "".
    found = [kv.partition("=")[2].lower() for kv in config.get("env", []) if kv.partition("=")[0] == "TLS_CERT_SHA256"]
    if len(found) != 1 or not found[0]:
        sys.exit("ask: the policy's launch must set TLS_CERT_SHA256 exactly once, to a value")
    return found[0], config["image"]


def verify(ip, policy_path):
    run = subprocess.run(["vcontainer", "verify", "--url", f"http://{ip}:7700", "--policy", policy_path],
                         capture_output=True, text=True)
    if run.returncode != 0:
        sys.exit(f"ask: verification failed; nothing was sent\n{run.stderr.strip()}")


def ask(ip, fingerprint, token, question):
    # The certificate is checked by its fingerprint from the verified launch,
    # not by a certificate authority, and before anything is sent.
    context = ssl.create_default_context()
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    conn = http.client.HTTPSConnection(ip, PORT, context=context, timeout=300)
    try:
        conn.connect()
    except OSError as e:
        sys.exit(f"ask: cannot reach {ip}:{PORT}: {e}")
    if hashlib.sha256(conn.sock.getpeercert(binary_form=True)).hexdigest() != fingerprint:
        conn.close()
        sys.exit("ask: the server's certificate is not the verified one; nothing was sent")
    conn.request("POST", "/ask", json.dumps({"question": question}),
                 {"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
    reply = json.loads(conn.getresponse().read())
    conn.close()
    return reply.get("answer") or f"error: {reply.get('error')}"


def main():
    p = argparse.ArgumentParser(description="Ask the private data analyst, after verifying it.")
    p.add_argument("--policy", required=True, help="the policy file from the custodian")
    p.add_argument("--token", required=True, help="the file holding the API token")
    p.add_argument("ip", help="the session's IP address")
    p.add_argument("question", nargs="?", help="one question; without it, ask interactively")
    args = p.parse_args()
    fingerprint, image = expected_fingerprint(args.policy)
    verify(args.ip, args.policy)
    print(f"verified: {image}\n  certificate {fingerprint[:16]}…")
    with open(args.token) as f:
        token = f.read().strip()
    if args.question:
        print(ask(args.ip, fingerprint, token, args.question))
        return
    print("Ask a question; quit with an empty line, quit or Ctrl-D.")
    while True:
        try:
            question = input("\nquestion> ").strip()
        except EOFError:
            break
        if question.lower() in ("", "quit", "exit"):
            break
        print(ask(args.ip, fingerprint, token, question))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print()
