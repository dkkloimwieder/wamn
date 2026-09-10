#!/usr/bin/env python3
"""Run the remaining scoped local gates once, with separate receipts."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
root = Path(__file__).resolve().parents[1]
assert json.loads((root / 'remint-001/result.json').read_text())['passed']
commands = [
    ('native-callers-001', ['env', 'CARGO_TARGET_DIR=' + str(tree / 'target'), 'cargo', 'test', '--manifest-path', 'components/Cargo.toml', '-p', 'wamn-receiving-data-access', '-p', 'wamn-client-acme-receiving-data-access', '--all-targets', '--no-fail-fast', '--locked', '--offline', '--', '--include-ignored', '--nocapture']),
    ('offline-sqlx-001', ['env', 'SQLX_OFFLINE=true', 'cargo', 'test', '-p', 'wamn-proof-conformance', '--test', 'receiving_sqlx_verifier', '--locked', '--offline', '--', '--include-ignored', '--nocapture']),
    ('clippy-001', ['cargo', 'clippy', '-p', 'wamn-schema-generator', '-p', 'wamn-proof-integration', '--all-targets', '--locked', '--offline']),
]
for name, command in commands:
    print('Stage: ' + name, flush=True)
    result = subprocess.run([sys.executable, str(root / 'tools/capture.py'), '--tree', str(tree), '--evidence-dir', str(root / name), '--', *command], cwd=tree)
    if result.returncode:
        raise SystemExit(result.returncode)
print('All scoped local gates completed.', flush=True)
