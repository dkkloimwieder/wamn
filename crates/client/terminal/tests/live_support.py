"""Shared private database calls and result files for live operator tests."""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys


class TestError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise TestError(message)


def load_terminal(root):
    sys.dont_write_bytecode = True
    path = root / "crates/client/terminal/tests/operator_pty.py"
    spec = importlib.util.spec_from_file_location("receiving_terminal_test", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module, path


class Evidence:
    def __init__(self, directory, secrets):
        directory.mkdir(parents=True, exist_ok=False)
        self.directory = directory
        self.secrets = sorted({value for value in secrets if value}, key=len, reverse=True)
        self.events = []

    def redact(self, text):
        for secret in self.secrets:
            text = text.replace(secret, "<redacted>")
        return text

    def write(self, name, text):
        (self.directory / name).write_text(self.redact(text))

    def json(self, name, value):
        self.write(name, json.dumps(value, indent=2, sort_keys=True) + "\n")

    def event(self, action, **fields):
        self.events.append({"action": action, **fields})
        self.json("commands.json", self.events)

    def sums(self):
        lines = []
        for path in sorted(self.directory.iterdir()):
            if path.is_file() and path.name != "SHA256SUMS":
                lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n")
        (self.directory / "SHA256SUMS").write_text("".join(lines))


class Database:
    def __init__(self, url, evidence):
        self.url, self.evidence = url, evidence

    def sql(self, name, sql, parse=False):
        self.evidence.write(name + ".sql", sql + "\n")
        self.evidence.event("psql", sql=name + ".sql", connection="private URL file via --dbname")
        environment = dict(os.environ, PGCONNECT_TIMEOUT="10")
        result = subprocess.run(
            ["psql", "--dbname", self.url, "-X", "-A", "-t", "-q", "-v", "ON_ERROR_STOP=1"],
            input=sql, text=True, capture_output=True, env=environment, timeout=30,
        )
        self.evidence.write(name + ".stdout", result.stdout)
        self.evidence.write(name + ".stderr", result.stderr)
        require(result.returncode == 0, f"psql failed at {name}; see redacted evidence")
        return json.loads(result.stdout) if parse else result.stdout.strip()
