#!/usr/bin/env python3
"""Restore Wasmtime's truncated guest names from matching compiled artifacts."""
import collections
import json
import pathlib
import re
import subprocess
import sys

artifacts, stacks = map(pathlib.Path, sys.argv[1:3])
candidates = collections.defaultdict(set)
# Wasmtime 48.0.5 compile.rs::clean_symbol retains 96 graphic ASCII bytes.
# Match both the function index and that exact prefix. Never guess collisions.
for artifact in sorted(artifacts.glob('*.wasm')):
    with subprocess.Popen(['wasm-tools', 'print', str(artifact)], stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            match = re.match(r'\s*\(func \$([^\s"]+) \(;(\d+);\)', line)
            if match and all(33 <= ord(c) <= 126 for c in match[1]):
                candidates[(match[2], match[1][:96])].add(match[1])
        if process.wait():
            raise SystemExit(f'Cannot read guest names from {artifact}')
full = sorted({next(iter(names)) for names in candidates.values() if len(names) == 1})
demangled = subprocess.run(['rustfilt'], input='\n'.join(full)+'\n', text=True, capture_output=True, check=True).stdout.splitlines()
if len(demangled) != len(full):
    raise SystemExit('Demangled guest names changed line count')
names = dict(zip(full, demangled))
counts = collections.Counter()


def restore(match):
    prefix, index, symbol = match.groups()
    symbol, _, offset = symbol.partition('+0x')
    found = candidates.get((index, symbol), set())
    if len(found) != 1:
        counts['ambiguous' if found else 'unmatched'] += 1
        return match[0]
    counts['resolved'] += 1
    return prefix + names[next(iter(found))] + ('+0x'+offset if offset else '')


raw = stacks.read_text()
with stacks.with_suffix('.raw.stacks').open('x') as destination:
    destination.write(raw)
stacks.write_text(re.sub(r'(wasm\[\d+\]::function\[(\d+)\]::)(\S+)', restore, raw))
stacks.with_suffix('.names.json').write_text(json.dumps(dict(counts), indent=2)+'\n')
