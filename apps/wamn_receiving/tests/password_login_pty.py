#!/usr/bin/env python3
"""Test hidden password enrollment, login, expiry and logout in the real terminal.

Run with --binary /path/to/wamn-receiving. Uses owned local HTTPS/HTTP fixtures.
"""
import argparse
import http.server
import importlib.util
import json
import os
from pathlib import Path
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("receiving_operator", Path(__file__).with_name("operator_pty.py"))
operator = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = operator
spec.loader.exec_module(operator)
terminal = operator.terminal
require = terminal.require
PASSWORD = "private-password-with-spaces"
INVITATION = "private-invitation-secret"
TOKEN = "private-password-session"
PRINCIPAL = "00000000-0000-0000-0000-000000000001"
AUDIENCE = "urn:wamn:project-env:acme:receiving:dev:fixture1"


class Identity:
    def __init__(self, directory):
        self.requests = []
        self.errors = []
        self.fail = False
        certificate = directory / "tls.crt"
        key = directory / "tls.key"
        result = subprocess.run([
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", str(key), "-out", str(certificate), "-days", "1",
            "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1",
            "-addext", "basicConstraints=critical,CA:FALSE",
        ], capture_output=True)
        require(result.returncode == 0, "fixture certificate creation failed")
        self.certificate = certificate
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= 8192, "authentication body exceeded its limit")
                    body = json.loads(self.rfile.read(length))
                    owner.requests.append(self.path)
                    require("authorization" not in self.headers, "password exchange unexpectedly used a PAT")
                    if self.path == "/password/enroll":
                        require(body == {"principal_id": PRINCIPAL, "invitation": INVITATION, "password": PASSWORD}, "enrollment body differed")
                        status, response = 204, b""
                    else:
                        require(self.path == "/password/session", "unexpected authentication route")
                        require(body == {"email": "alice@example.invalid", "password": PASSWORD, "aud": AUDIENCE}, "login body differed")
                        status = 401 if owner.fail else 200
                        response = json.dumps({"access_token": TOKEN, "token_type": "Bearer", "expires_at": int(time.time()) + 3}).encode()
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except Exception:
                    owner.errors.append("identity fixture failed")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05})
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def environment(self):
        return {"WAMN_SESSION_ISSUER": f"https://127.0.0.1:{self.server.server_port}", "WAMN_SESSION_AUDIENCE": AUDIENCE, "WAMN_SESSION_CA": str(self.certificate)}


def answer(session, label, value, paste=False):
    session.text(label)
    session.send((b"\x1b[200~" + value.encode() + b"\x1b[201~" if paste else value.encode()) + b"\r")


def secret_free(session):
    for secret in (PASSWORD, INVITATION, TOKEN):
        require(secret.encode() not in session.output, "terminal exposed a secret")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="wamn-password-pty-") as directory:
        identity = Identity(Path(directory))
        fixture = operator.Fixture()
        try:
            def session():
                return terminal.Session(binary, ROOT, fixture, "password-fixture", operator.HOST, None, identity.environment())
            with session() as terminal_session:
                answer(terminal_session, "Enter L", "i")
                answer(terminal_session, "Principal ID", PRINCIPAL)
                answer(terminal_session, "Invitation secret", INVITATION, True)
                answer(terminal_session, "New password", PASSWORD, True)
                answer(terminal_session, "Confirm new password", PASSWORD, True)
                answer(terminal_session, "Email:", "alice@example.invalid")
                answer(terminal_session, "Password (hidden):", PASSWORD, True)
                terminal_session.text("PTY-FIRST-ORDER")
                require(fixture.snapshot()[0].headers.get("authorization") == f"Bearer {TOKEN}", "ordinary operation did not use the password session")
                require(identity.requests == ["/password/enroll", "/password/session"], "authentication repeated unexpectedly")
                deadline = time.monotonic() + 4
                while time.monotonic() < deadline:
                    terminal_session.pump()
                terminal_session.send(b"\x1b[15~")
                terminal_session.text("Authentication required")
                terminal_session.quiet(1)
                terminal_session.send(b"q")
                terminal_session.finish()
                require(b"Logged out locally" in terminal_session.output, "logout was not reported")
                secret_free(terminal_session)
            before = list(identity.requests)
            for stop in (b"\x03", signal.SIGTERM):
                with session() as terminal_session:
                    answer(terminal_session, "Enter L", "l")
                    answer(terminal_session, "Email:", "alice@example.invalid")
                    terminal_session.text("Password (hidden):")
                    terminal_session.send(PASSWORD.encode())
                    terminal_session.pump()
                    if isinstance(stop, bytes):
                        terminal_session.send(stop)
                    else:
                        os.kill(terminal_session.process.pid, stop)
                    terminal_session.finish(exit_code=0 if isinstance(stop, bytes) else 143)
                    secret_free(terminal_session)
            require(identity.requests == before, "cancelled prompt sent credentials")
            identity.fail = True
            with session() as terminal_session:
                answer(terminal_session, "Enter L", "l")
                answer(terminal_session, "Email:", "alice@example.invalid")
                answer(terminal_session, "Password (hidden):", PASSWORD)
                terminal_session.finish(exit_code=1)
                secret_free(terminal_session)
            require(len(identity.requests) == len(before) + 1, "failed login retried")
            require(len(fixture.snapshot()) == 1, "login failure or expiry sent an application request")
            require(not identity.errors, "identity fixture failed")
        except terminal.TestError:
            if "terminal_session" in locals():
                diagnostic = terminal_session.output.decode(errors="replace")[-1600:]
                for secret in (PASSWORD, INVITATION, TOKEN):
                    diagnostic = diagnostic.replace(secret, "[redacted]")
                print(repr(diagnostic), file=sys.stderr)
            raise
        finally:
            fixture.close()
            identity.close()
    print("Password PTY: enrollment, login, expiry, logout, cancellation, failure and secret hiding passed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    try:
        run(args.binary.resolve())
    except terminal.TestError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from None
    except Exception:
        print("Password PTY failed; captured secrets are suppressed.", file=sys.stderr)
        raise SystemExit(1) from None
