#!/usr/bin/env python3
"""Use the real terminal against the owned Receiving and identity services."""
import argparse
import http.server
import os
import subprocess
import tempfile
import threading
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
import password_login_pty as login


class Application:
    def __init__(self, url):
        self.url = url

    def snapshot(self):
        return []


def capture_mail(command):
    with tempfile.TemporaryDirectory(prefix="wamn-invitation-") as directory:
        capture = Path(directory) / "mail.json"
        errors = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    login.require(self.path == "/emails", "unexpected email path")
                    login.require(self.headers.get("Authorization") == "Bearer fixture-key", "unexpected mail credential")
                    length = int(self.headers.get("Content-Length", "0"))
                    login.require(0 < length <= 8192, "email exceeded fixture limit")
                    body = json.loads(self.rfile.read(length))
                    login.require(body["to"] == ["managed-development@example.invalid"], "email account binding differs")
                    login.require(body["from"] == "WAMN <fixture@example.invalid>", "email sender differs")
                    with os.fdopen(os.open(capture, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as output:
                        json.dump(body, output)
                    self.send_response(200)
                    self.send_header("Content-Length", "2")
                    self.end_headers()
                    self.wfile.write(b"{}")
                except Exception:
                    errors.append("invitation mail capture failed")
                    self.send_error(500)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05})
        thread.start()
        try:
            environment = os.environ | {
                "WAMN_TEST_RESEND_ENDPOINT": f"http://127.0.0.1:{server.server_port}/emails",
                "WAMN_TEST_INVITATION_FILE": str(capture),
                "RESEND_API_KEY": "unused-development-fixture",
                "RESEND_FROM": "WAMN <fixture@example.invalid>",
            }
            result = subprocess.run(command, env=environment)
            login.require(result.returncode == 0, "invitation journey command failed")
            login.require(capture.exists() and not errors, "invitation mail was not captured")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    facts = json.load(sys.stdin)
    environment = {
        "WAMN_SESSION_ISSUER": facts["issuer"],
        "WAMN_SESSION_AUDIENCE": facts["audience"],
        "WAMN_SESSION_CA": facts["ca"],
    }
    with login.terminal.Session(args.binary, login.ROOT, Application(facts["url"]),
                                facts["instance"], facts["host"], None, environment) as session:
        login.answer(session, "Enter L", "i")
        login.answer(session, "Principal ID", facts["principal"])
        login.answer(session, "Invitation secret", facts["invitation"], True)
        login.answer(session, "New password", facts["password"], True)
        login.answer(session, "Confirm new password", facts["password"], True)
        login.answer(session, "Email:", facts["email"])
        login.answer(session, "Password (hidden):", facts["password"], True)
        session.text("PASSWORD-JOURNEY")
        session.send(b"q")
        session.finish()
        login.require(facts["invitation"].encode() not in session.output, "terminal exposed the invitation")
        login.require(facts["password"].encode() not in session.output,
                      "terminal exposed the password")
        login.require(b"Logged out locally" in session.output, "local logout was not reported")
    print("Password login and authorized Receiving query passed without a PAT.")


if __name__ == "__main__":
    try:
        if sys.argv[1:2] == ["--capture-mail"]:
            capture_mail(sys.argv[2:])
        else:
            main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
