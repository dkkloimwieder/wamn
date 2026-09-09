"""Evidence and child-process helpers reused from the offline-validated authority preparation."""
import argparse

import hashlib

import json

import os

from pathlib import Path

import re

import secrets

import signal

import subprocess

import sys

import time

SUMMARY = re.compile(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;.*$', re.M)

SAFE_LINE = re.compile(r'^(?:running \d+ tests?|test [A-Za-z0-9_:]+ \.\.\. (?:ok|FAILED)|test result: (?:ok|FAILED)\. \d+ passed; \d+ failed; \d+ ignored; \d+ measured; \d+ filtered out; finished in [0-9.]+s)$')

SKIP = re.compile(r'(?im)^(?:test [A-Za-z0-9_:]+ \.\.\. )?(?:skipping\b|skipped\b|skip\s*:|self[- ]skip\b)')

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.chmod(0o644)

def public_copy(raw, output):
    # Existing tests mint additional credentials. Preserve only Rust test protocol
    # lines, not arbitrary diagnostics whose secret values we cannot enumerate.
    lines = raw.read_text(errors='replace').splitlines()
    output.write_text('\n'.join(line if SAFE_LINE.fullmatch(line) else '[private diagnostic redacted]' for line in lines) + '\n')
    output.chmod(0o644)

def clean_source(repo, source):
    head = subprocess.run(['git', '-C', str(repo), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
    status = subprocess.run(['git', '-C', str(repo), 'status', '--porcelain=v1', '--untracked-files=normal'], capture_output=True, text=True, check=True).stdout
    if head != source or status:
        raise RuntimeError('Source must remain clean and equal the requested full commit')
    return {'commit': head, 'clean': True}

class Runner:
    def __init__(self, repo, evidence):
        self.repo = repo
        self.evidence = evidence
        self.private = evidence / 'private'
        self.private.mkdir(mode=0o700)
        self.commands = []
        self.environment = {k: v for k, v in os.environ.items() if not k.startswith(('WAMN_', 'PG')) and k != 'DATABASE_URL'}

    def command(self, label, argv, environment=None, timeout=1800):
        number = len(self.commands) + 1
        stem = f'{number:03d}-{label}'
        raw = self.private / (stem + '.log')
        output = self.evidence / (stem + '.redacted.log')
        receipt = {'label': label, 'argv': argv, 'cwd': str(self.repo), 'started_unix_ns': time.time_ns(), 'exit_code': None, 'timeout': False, 'interrupted': False, 'raw_private_log': str(raw.relative_to(self.evidence)), 'redacted_log': output.name}
        self.commands.append(receipt)
        error = None
        child = None
        try:
            with raw.open('xb') as log:
                raw.chmod(0o600)
                child = subprocess.Popen(argv, cwd=self.repo, env=environment or self.environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    receipt['exit_code'] = child.wait(timeout=timeout)
                except (subprocess.TimeoutExpired, KeyboardInterrupt) as exc:
                    receipt['timeout'] = isinstance(exc, subprocess.TimeoutExpired)
                    receipt['interrupted'] = isinstance(exc, KeyboardInterrupt)
                    error = exc
                finally:
                    try:
                        os.killpg(child.pid, 0)
                        group_remains = True
                    except ProcessLookupError:
                        group_remains = False
                    receipt['owned_group_cleanup'] = group_remains
                    if group_remains:
                        # The whole owned process group drains before the fixture is removed.
                        try:
                            os.killpg(child.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        deadline = time.monotonic() + 20
                        while time.monotonic() < deadline:
                            child.poll()
                            try:
                                os.killpg(child.pid, 0)
                            except ProcessLookupError:
                                break
                            time.sleep(0.1)
                        else:
                            try:
                                os.killpg(child.pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        receipt['exit_code'] = child.wait()
        except OSError as exc:
            error = exc
            receipt['launch_error'] = type(exc).__name__
        finally:
            receipt['finished_unix_ns'] = time.time_ns()
            if raw.exists():
                public_copy(raw, output)
                receipt['raw_sha256'] = digest(raw)
                receipt['redacted_sha256'] = digest(output)
            write_json(self.evidence / 'commands.json', self.commands)
        if isinstance(error, KeyboardInterrupt):
            raise error
        return receipt, raw.read_text(errors='replace') if raw.exists() else ''

    def required(self, label, argv, environment=None, timeout=120):
        receipt, text = self.command(label, argv, environment, timeout)
        if receipt['exit_code'] != 0 or receipt['timeout'] or 'launch_error' in receipt:
            raise RuntimeError('Required setup command failed: ' + label)
        return text
