#!/usr/bin/env python3
"""Run the retained Receiving recipe with existing B artifacts and a libtest binary."""
import argparse
import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import unquote, urlsplit

LANE = Path('/home/kaalin/.cache/wamn-lanes/native-b-adoption-20260910')
EVIDENCE = Path('/home/kaalin/dev/wamn/docs/perf/2026.09/native-b-adoption')
AUTHORITY = LANE / 'docs/operations/build-and-test.md'
TEST_ARGS = ['route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes',
             '--ignored', '--exact', '--nocapture', '--test-threads=1']


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def shell_recipe():
    section = AUTHORITY.read_text().split('### `[RECEIVING-ROUTE-JOURNEY]`', 1)[1].split('\n### ', 1)[0]
    recipe = next(block for block in re.findall(r'```bash\n(.*?)\n```', section, re.S)
                  if block.startswith('set -euo pipefail')) + '\n'

    def replace(old, new):
        nonlocal recipe
        if recipe.count(old) != 1:
            raise ValueError('Retained Receiving recipe changed; inspect before adapting')
        recipe = recipe.replace(old, new, 1)

    replace('RECEIVING_ROUTE_SCRATCH="$(mktemp -d /tmp/wamn-receiving-route.XXXXXX)"',
            'RECEIVING_ROUTE_SCRATCH="$WAMN_CAPTURE_SCRATCH"')
    replace('RECEIVING_ROUTE_PROJECT="wamn-receiving-route-$$"',
            'RECEIVING_ROUTE_PROJECT="$WAMN_CAPTURE_PROJECT"')
    replace('CARGO_TARGET_DIR="$RECEIVING_ROUTE_SCRATCH/target" \\\n  "$RECEIVING_ROUTE_ROOT/tools/build-components" m1\n', '')
    recipe = recipe.replace('$RECEIVING_ROUTE_SCRATCH/target/', '$RECEIVING_ROUTE_ROOT/target/')
    replace('cargo build -p wamn-scenario-worker --locked --offline\n', '')
    replace('cargo build -p wamn-identity --bin wamn-identity --locked --offline\n', '')
    replace('  cargo test -p wamn-proof-integration --lib --locked --offline \\\n  route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes \\\n  -- --ignored --exact --nocapture --test-threads=1', '  "$@"')
    replace('    >/dev/null 2>&1 || true',
            '    >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.log" 2>&1 \\\n    && printf "0\\n" >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.exit" \\\n    || printf "%s\\n" "$?" >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.exit"')
    replace('  if [[ "$RECEIVING_ROUTE_SCRATCH" == /tmp/wamn-receiving-route.* ]]; then\n'
            '    rm -rf -- "$RECEIVING_ROUTE_SCRATCH"\n  fi',
            '  # The capture wrapper scans private output, then removes this exact scratch directory.')
    return recipe


def redact(raw, scratch):
    secrets = set()

    def inspect(value, key=''):
        if isinstance(value, dict):
            for name, item in value.items():
                if name == 'data' and isinstance(item, dict):
                    for encoded in item.values():
                        if isinstance(encoded, str):
                            try:
                                inspect(base64.b64decode(encoded, validate=True).decode(), 'decoded_secret')
                            except (ValueError, UnicodeDecodeError):
                                pass
                inspect(item, name)
        elif isinstance(value, list):
            for item in value:
                inspect(item, key)
        elif isinstance(value, str):
            if key.lower() in ('password', 'token', 'pat', 'secret', 'decoded_secret') and len(value) > 8:
                secrets.add(value.encode())
            secrets.update(match.encode() for match in re.findall(r'wamn_pat_[0-9a-f]{16}_[0-9a-f]{64}', value))
            for url in re.findall(r'postgres(?:ql)?://[^\s"<>]+', value):
                password = urlsplit(url).password
                if password and password != 'probe':
                    secrets.add(password.encode())
                    secrets.add(unquote(password).encode())

    for path in [scratch / '.dockerconfigjson', scratch / 'route-caller-pat.json',
                 *(scratch / 'host-secrets').glob('**/*.json')]:
        if path.is_file():
            inspect(json.loads(path.read_text()))
    auth_path = scratch / '.dockerconfigjson'
    if auth_path.is_file():
        for entry in json.loads(auth_path.read_text())['auths'].values():
            secrets.add(base64.b64encode(f"{entry['username']}:{entry['password']}".encode()))
    inspect(raw.decode('utf-8', errors='replace'))
    changes = []
    for secret in sorted(secrets, key=len, reverse=True):
        count = raw.count(secret)
        if count:
            raw = raw.replace(secret, b'[REDACTED_EPHEMERAL_CREDENTIAL]')
            changes.append({'occurrences': count, 'credential_bytes': len(secret)})
    return raw, changes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-name', required=True)
    parser.add_argument('binary_argv', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.binary_argv[1:] if args.binary_argv[:1] == ['--'] else args.binary_argv
    if not re.fullmatch('[a-z0-9][a-z0-9-]*', args.run_name):
        parser.error('Use a simple fresh evidence run name')
    if not command or command[1:] != TEST_ARGS:
        parser.error('Supply the absolute libtest binary followed by the exact Receiving test and required flags')
    binary = Path(command[0])
    if not binary.is_absolute() or not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error('The explicit libtest binary must be an absolute executable file')
    evidence = EVIDENCE / args.run_name
    if evidence.exists():
        parser.error('Evidence directory must be fresh')
    artifacts = [binary, LANE / 'target/debug/wamn-scenario-worker', LANE / 'target/debug/wamn-identity',
                 LANE / 'target/wasm32-wasip2/release/http_route.wasm',
                 *sorted((LANE / 'target/virtualized/std-empty-environment').glob('*.wasm'))]
    if not all(path.is_file() and path.stat().st_size for path in artifacts):
        parser.error('A required existing binary or component is missing')
    recipe = shell_recipe()
    env = {name: value for name, value in os.environ.items()
           if not name.startswith(('WAMN_', 'WASH_', 'OTEL_', 'PG', 'GIT_'))
           and name not in ('DATABASE_URL', 'DB_URL', 'NATS_URL', 'CARGO', 'CARGO_TARGET_DIR',
                            'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'BASH_ENV', 'ENV', 'SHELLOPTS', 'BASHOPTS')}
    env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1', GIT_OPTIONAL_LOCKS='0',
               KUBECONFIG='/dev/null', RUST_BACKTRACE='1')

    def git(*argv):
        return subprocess.check_output(['git', *argv], cwd=LANE, env=env)

    def source():
        paths = git('ls-files', '--cached', '--others', '--exclude-standard', '-z').decode().split('\0')
        return {'head': git('rev-parse', 'HEAD').decode().strip(),
                'status': git('status', '--porcelain=v1', '--untracked-files=all').decode(),
                'sha256': {path: sha(LANE / path) for path in paths if path and
                           not path.startswith(('.beads/', 'docs/perf/')) and (LANE / path).is_file()},
                'artifacts_sha256': {str(path): sha(path) for path in artifacts}}

    os.umask(0o077)
    evidence.mkdir(parents=True, exist_ok=False)
    (evidence / 'recipe.sh').write_text(recipe)
    (evidence / 'capture.py').write_bytes(Path(__file__).read_bytes())
    write(evidence / 'command.json', {'cwd': str(LANE), 'argv': ['bash', str(evidence / 'recipe.sh'), *command],
                                    'authority': str(AUTHORITY), 'authority_sha256': sha(AUTHORITY)})
    before = source()
    write(evidence / 'source-before.json', before)
    (evidence / 'source.patch').write_bytes(git('diff', 'HEAD', '--binary', '--', '.', ':!.beads', ':!docs/perf'))
    scratch = Path(tempfile.mkdtemp(prefix='wamn-receiving-route.', dir='/tmp'))
    project = 'wamn-receiving-route-' + scratch.name.rsplit('.', 1)[1].lower()
    env.update(WAMN_CAPTURE_SCRATCH=str(scratch), WAMN_CAPTURE_PROJECT=project, TMPDIR=str(scratch))
    write(evidence / 'environment-names.json', {'names': sorted(env), 'values_recorded': False})
    started = time.monotonic()
    result = {'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'exit_code': 125}
    write(evidence / 'result.json', result)
    cleanup = {'compose_project': project, 'scratch': str(scratch)}
    try:
        with (scratch / 'private.log').open('wb') as output:
            result['exit_code'] = subprocess.run(['bash', str(evidence / 'recipe.sh'), *command],
                                                 cwd=LANE, env=env, stdout=output, stderr=subprocess.STDOUT).returncode
    finally:
        try:
            for name, destination in [('private.log', 'output.log'), ('compose-cleanup.log', 'cleanup.log')]:
                if (scratch / name).exists():
                    clean, changes = redact((scratch / name).read_bytes(), scratch)
                    (evidence / destination).write_bytes(clean)
                    write(evidence / (destination + '.redaction.json'), {
                        'scan': 'Actual serialized fixture credentials, registry Basic auth, full PAT values, and PostgreSQL URL passwords; no blanket line removal.',
                        'redactions': changes, 'output_sha256': sha(evidence / destination)})
            status = scratch / 'compose-cleanup.exit'
            cleanup['compose_down_exit_code'] = int(status.read_text()) if status.exists() else None
            after = source()
            write(evidence / 'source-after.json', after)
            write(evidence / 'source-stability.json', {'same_head': before['head'] == after['head'],
                'same_source_hashes': before['sha256'] == after['sha256'],
                'same_artifact_hashes': before['artifacts_sha256'] == after['artifacts_sha256'],
                'same_status': before['status'] == after['status']})
        finally:
            shutil.rmtree(scratch)
            cleanup['scratch_removed'] = not scratch.exists()
            write(evidence / 'cleanup.json', cleanup)
            result.update(finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                          elapsed_seconds=time.monotonic() - started)
            write(evidence / 'result.json', result)
            (evidence / 'exit-code.txt').write_text(str(result['exit_code']) + '\n')
    return result['exit_code'] or (0 if cleanup.get('compose_down_exit_code') == 0 else 1)


if __name__ == '__main__':
    raise SystemExit(main())
