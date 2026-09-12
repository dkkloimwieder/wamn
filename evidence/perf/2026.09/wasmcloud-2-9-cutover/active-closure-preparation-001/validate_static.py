#!/usr/bin/env python3
"""Offline scope and outer SELECT-list checks, not a PostgreSQL execution test."""
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

here = Path(__file__).resolve().parent
worktree = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
source = worktree / 'crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs'
proposed = here / 'wiring_resolution.rs.proposed'
original = source.read_text()
changed = proposed.read_text()


def constant(text, name):
    prefix = 'pub const ' + name + ': &str = '
    start = text.index(prefix) + len(prefix)
    end = text.index('";', start) + 1
    literal = text[start:end]
    return json.loads(re.sub(r'\\\n\s*', '', literal))


def outer_positions(sql):
    depth = 0
    quoted = None
    index = 0
    while index < len(sql):
        char = sql[index]
        if quoted:
            if char == quoted:
                if index + 1 < len(sql) and sql[index + 1] == quoted:
                    index += 2
                    continue
                quoted = None
            index += 1
            continue
        if char in ('\'', '"'):
            quoted = char
            index += 1
            continue
        if char == '(':
            depth += 1
        elif char == ')':
            depth -= 1
            assert depth >= 0
        elif depth == 0:
            if char == ',':
                yield index, ','
            elif char.isalpha() or char == '_':
                end = index + 1
                while end < len(sql) and (sql[end].isalnum() or sql[end] == '_'):
                    end += 1
                yield index, sql[index:end].upper()
                index = end
                continue
        index += 1
    assert depth == 0 and quoted is None


def columns(sql):
    positions = list(outer_positions(sql))
    start = next(position + len(word) for position, word in positions if word == 'SELECT')
    end = next(position for position, word in positions if position > start and word == 'FROM')
    separators = [position for position, word in positions if start < position < end and word == ',']
    edges = [start - 1] + separators + [end]
    return [' '.join(sql[edges[i] + 1:edges[i + 1]].split()) for i in range(len(edges) - 1)]


names = ('ACTIVE_WIRING_SQL', 'RELEASE_WIRING_SQL', 'CANDIDATE_WIRING_SQL')
before = {name: constant(original, name) for name in names}
after = {name: constant(changed, name) for name in names}
projection = {name: {'before': columns(before[name]), 'after': columns(after[name])} for name in names}
start = original.index('pub const ACTIVE_WIRING_SQL:')
end = original.index('\n\n/// The immutable-version snapshot', start)
new_start = changed.index('pub const ACTIVE_WIRING_SQL:')
new_end = changed.index('\n\n/// The immutable-version snapshot', new_start)
active = after['ACTIVE_WIRING_SQL']
checks = {
    'only_active_constant_changed': original[:start] == changed[:new_start] and original[end:] == changed[new_end:],
    'active_eight_columns': len(projection['ACTIVE_WIRING_SQL']['after']) == 8,
    'old_active_six_columns': len(projection['ACTIVE_WIRING_SQL']['before']) == 6,
    'first_six_columns_unchanged': projection['ACTIVE_WIRING_SQL']['before'] == projection['ACTIVE_WIRING_SQL']['after'][:6],
    'closure_columns_match_frozen_query': projection['ACTIVE_WIRING_SQL']['after'][6:] == [value.replace('selected.release_id', 'selected.effective_release_id') for value in projection['RELEASE_WIRING_SQL']['after'][6:]],
    'frozen_and_candidate_sql_unchanged': all(before[name] == after[name] for name in names[1:]),
    'active_five_bindings_unchanged': set(re.findall(r'\$\d+', before['ACTIVE_WIRING_SQL'])) == set(re.findall(r'\$\d+', active)) == {'$1', '$2', '$3', '$4', '$5'},
    'snapshot_head_identity_preserved': 'snapshot.tenant_id = head.tenant_id' in active and 'snapshot.effective_release_id = head.effective_release_id' in active,
    'snapshot_environment_checked': "#>> '{release,environment}' = $3" in active,
    'activation_checks_preserved': all(predicate in active for predicate in (
        'head.tenant_id = active.tenant_id', 'head.environment = active.environment',
        'member.effective_release_id = head.effective_release_id',
        'wiring.package_version = member.package_version',
        'wiring.version = $5', 'wiring.wiring_hash = active.confirmed_definition_hash',
        'active.tenant_id = $1', 'active.package_id = $2', 'active.environment = $3',
        'active.wiring_id = $4', 'AND active.enabled',
        'dead.tenant_id = active.tenant_id', 'dead.package_id = active.package_id',
        'dead.environment = active.environment', 'dead.wiring_id = active.wiring_id')),
}
assert all(checks.values()), checks
for name in names:
    (here / (name.lower() + '.sql')).write_text(after[name] + '\n')
ast.parse((here / 'prepare.py').read_text())
commands = []
for argv in (
    ['rustfmt', '--edition', '2024', '--check', str(proposed)],
    ['git', 'apply', '--check', str(here / 'proposal.patch')],
):
    proc = subprocess.run(argv, cwd=worktree, env={**os.environ, 'RUSTUP_TOOLCHAIN': '1.98.0'}, capture_output=True, text=True)
    commands.append({'argv': argv, 'exit_code': proc.returncode, 'stdout': proc.stdout, 'stderr': proc.stderr})
    assert proc.returncode == 0, commands[-1]
result = {
    'scope': 'Offline source/SELECT-list checks and rustfmt only. This is not SQL parsing, SQL execution, compilation, or live proof.',
    'checks': checks,
    'projection_columns': projection,
    'commands': commands,
    'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
    'proposed_sha256': hashlib.sha256(proposed.read_bytes()).hexdigest(),
}
(here / 'static-validation.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'checks': checks, 'command_exits': [c['exit_code'] for c in commands]}))
