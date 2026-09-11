#!/usr/bin/env python3
"""Finish B's report from completed receipts without changing production source."""
import json
from pathlib import Path
import subprocess

TREE = Path('/home/kaalin/.cache/wamn-lanes/native-b-adoption-20260910')
BASE = '79879412aceba021a5f77e273e6c1a21dff7dc34'
SOURCE = '07c7a858c4452579b12e0ae25a4e61af2107c4eb'
ROOT = TREE / 'docs/perf/2026.09/native-b-adoption'

inventory = ROOT / 'production-code-inventory-002'
inventory.mkdir(exist_ok=False)
argv = ['git', 'diff', '--numstat', BASE, SOURCE, '--', '.', ':(exclude)docs/perf']
raw = subprocess.check_output(argv, cwd=TREE, text=True)
rows = []
for line in raw.splitlines():
    added, deleted, path = line.split('\t', 2)
    rows.append({'path': path, 'added': int(added), 'deleted': int(deleted)})
(inventory / 'command.json').write_text(json.dumps(argv, indent=2) + '\n')
(inventory / 'numstat.txt').write_text(raw)
data = {'base': BASE, 'source': SOURCE,
        'scope': 'Changed files excluding proof receipts. Counts mix implementation, tests, and documentation; they are not production-only line counts.',
        'files': rows, 'added': sum(x['added'] for x in rows),
        'deleted': sum(x['deleted'] for x in rows)}
(inventory / 'inventory.json').write_text(json.dumps(data, indent=2) + '\n')

path = ROOT / 'production-report.md'
text = path.read_text()
replacements = {
    'Owner: `wamn-0ct2.2`. Status: implementation and validation remain in progress.\nThis report records the corrected build, host tests, authenticated trace proof, and subsequent direct and nested HTTP proofs.\nEach passing row describes its recorded source and artifacts. These focused results do not establish a final passing B landing.':
    'Owner: `wamn-0ct2.2`. The production substitution and its owning correctness proofs are complete.\nOne implementation per imported operation interface replaces selection of a provider for each dependency.\nNative execution replaces the manual implementation for released, nested, and candidate calls.\nThe final workspace sweep retains 83 baseline failures and one unarmed fixture failure, which passes in its separate armed proof.\nEach result below names its actual source and artifacts. Git and Beads record publication status.',
    'Final landed source and artifact identities remain pending.':
    'Commit `6feb01aca9c8fcced0c4ec9d3f5958416d2cf371` introduces the production substitution.\nCommit `6f02d70d6b0a03b90160652df2d9c16c065010e3` corrects the WASI plugin declaration and supplies the deployed proof source.\nCommit `07c7a858c4452579b12e0ae25a4e61af2107c4eb` corrects only the direct-consumer test inventory.\nThe final main sweep uses `07c7a858`, with unchanged production source from `6f02d70d`.\nThe final documentation and evidence commit adds no production changes.',
    'The deployed Receiving result is recorded in this report. Integrated validation remains pending.':
    'The deployed Receiving proof passes. The [final workspace result](#final-workspace-result) records the remaining baseline and fixture failures.',
    'The deployed Receiving result is recorded below. The integrated workspace sweep remains pending.':
    'The deployed Receiving and final workspace results follow below.',
    'The integrated workspace sweep remains pending.':
    'The [final workspace result](#final-workspace-result) records the completed sweep and its limits.'
}
for old, new in replacements.items():
    assert old in text, old
    text = text.replace(old, new)

section = '''## Final workspace result

The [final main sweep](integrated-workspace-002/run.json) runs at `07c7a858c4452579b12e0ae25a4e61af2107c4eb` and exits 101 in 114.18 seconds.
Its [source receipt](integrated-workspace-002/source-stability.json) records no changes during execution.
The [results](integrated-workspace-002/workspace-results.json) report 2,245 test passes, six doctest passes, 84 failures, zero ignored tests, and two filtered schema regenerators.
The 85 explicit self-skips remain unchanged from the baseline.
Subtracting those skips leaves 2,166 reported passes, including doctests, but does not establish an exact executed count.

The [comparison](integrated-workspace-002/workspace-comparison.json) uses the latest nested-authority baseline at `dd28c68cadf825fe9eec44f7ae2e24ab28b89442`.
The [classification](workspace-comparison-002/classification.json) retains the exact causes and command provenance.
Eighty-two failure identities and causes match exactly.
One existing WIT-parser failure differs only in eight checkout path prefixes.
The only added failure requires `WAMN_NATIVE_B_AUTH_PG_URL`, which the unarmed sweep deliberately omits.
No test target or explicit self-skip identity changes.

The [first sweep](integrated-workspace-001/workspace-results.json) reports two additional inventory failures.
Its [classification](workspace-comparison-001/classification.json) retains those introduced failures as failures.
Commit `07c7a858` removes the obsolete direct `wasmtime-wasi` consumer row without weakening the source-universe assertions.
The [focused inventory proof](production-source-identity-001/output.log) passes all three tests.
The final sweep also passes all three inventory tests.

The [separate armed proof](production-authenticated-main-001/output.log) uses the main binary built from production source `6f02d70d6b0a03b90160652df2d9c16c065010e3`.
Its SHA-256 is `3d72e55a7f6f756e21d2d701e164e69e2d7c8355e318098178a471d68e0d52bb`.
It passes one test, with zero failures, zero ignored tests, and 52 filtered tests in 2.41 seconds.
All six cases pass: permission refusal, fresh-only refusal, success, initialization deadline, execution deadline, and cancellation.
The trace contains exactly two invocation spans and two host observations with the required parent relationships.
Its [command](production-authenticated-main-001/command.json) and [result](production-authenticated-main-001/result.json) retain exact invocation and unchanged source.
The log records removal of its exact owned PostgreSQL container.
This separately armed result does not turn the unarmed workspace sweep into a passing run.

## Removed code and remaining deviations

The [final code inventory](production-code-inventory-002/inventory.json) compares published base `79879412` with tested source `07c7a858`.
Its counts include implementation, tests, and documentation. They do not measure production-only code size.
Native loading and dispatch replace the manual compiled cache, prepared cache, component linker, node instances, and store lifecycle.
The same landing removes the obsolete manual epoch and store guards, direct WASI dependency, and replaced trace expectations.

WAMN retains invocation authority, exact component provenance, statement authorization, wiring, truthful outcomes, and the enclosing deadline.
Request authority ends with each invocation. Every call still uses a fresh store.
Positive `poolSize` remains refused until B2 proves reuse, isolation, overflow, and retirement with `maxConcurrency = 1`.
C remains a separate unfinished substitution. D retains resolved-IP enforcement, and F retains the exclusive PostgreSQL session implementation.
Issues `.74` through `.76` remain open under their existing scope.

Both compared sources use unmodified wasmCloud 2.9 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` and Wasmtime 47.0.4.
The final comparison records correctness against that identified baseline.
No benchmark runs for this landing, and this report claims no performance improvement.
The owner excludes a benchmark prerequisite. No additional performance gate blocks B.

'''
assert '## Final workspace result' not in text
text = text.replace('## Cleanup and remaining evidence\n', section + '## Cleanup and remaining evidence\n')
old = '''The full workspace sweep runs after source integration, as the owning recipe requires. It does not run inside the worktree lane.
The final report must tie the owning release, nested, candidate, and deployed proofs to their actual source and artifact identities.
It must retain exact commands, counts, cleanup, remaining deviations, and the required identified 2.9 comparison.
This report claims no new performance measurement or completed landing.'''
new = '''The full workspace sweep runs on main after source integration, as the owning recipe requires.
The linked receipts retain commands, counts, source and artifact identities, cleanup, remaining deviations, and the identified 2.9 correctness comparison.
No owned B build or live fixture remains active.
Worktree cleanup follows publication of both Git and Dolt records. Publication status lives in Beads and Git.'''
assert old in text
text = text.replace(old, new)
path.write_text(text)
print(json.dumps({'changed_report': str(path), 'inventory_files': len(rows),
                  'added': data['added'], 'deleted': data['deleted']}, indent=2))
