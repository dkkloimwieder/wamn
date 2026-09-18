#!/usr/bin/env python3
"""Use the real terminal against the owned Receiving and identity services."""
import argparse
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
        login.answer(session, "Enter L", "l")
        login.answer(session, "Email:", facts["email"])
        login.answer(session, "Password (hidden):", facts["password"], True)
        session.text("PASSWORD-JOURNEY")
        session.send(b"q")
        session.finish()
        login.require(facts["password"].encode() not in session.output,
                      "terminal exposed the password")
        login.require(b"Logged out locally" in session.output, "local logout was not reported")
    print("Password login and authorized Receiving query passed without a PAT.")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
