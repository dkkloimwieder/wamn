#!/usr/bin/env python3
"""Prepare the missing active-wiring row columns; never edit source."""
import difflib
import hashlib
import json
from pathlib import Path

here = Path(__file__).resolve().parent
worktree = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
relative = Path('crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs')
source = worktree / relative
original = source.read_text()
start = original.index('pub const ACTIVE_WIRING_SQL:')
end = original.index('\n\n/// The immutable-version snapshot', start)
active = original[start:end]
release_start = original.index('pub const RELEASE_WIRING_SQL:')
release_end = original.index('\n\n/// Exact immutable candidate wiring', release_start)
release = original[release_start:release_end]
changed = active


def replace(old, new):
    global changed
    assert changed.count(old) == 1, old
    changed = changed.replace(old, new)


replace(r'''           wiring.graph_json, wiring.wiring_hash \
''', r'''           wiring.graph_json, wiring.wiring_hash, \
           convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb AS manifest \
''')
replace(r'''       AND head.environment = active.environment \
''', r'''       AND head.environment = active.environment \
      JOIN catalog.release_manifest_v3_snapshots AS snapshot \
        ON snapshot.tenant_id = head.tenant_id \
       AND snapshot.effective_release_id = head.effective_release_id \
       AND convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb \
             #>> '{release,environment}' = $3 \
''')
closure_start = release.index('       COALESCE(', release.index(')::text AS node_components,'))
closure_end = release.index('  FROM selected', closure_start)
closure = release[closure_start:closure_end]
# Reuse the existing complete released closure. ACTIVE calls the same release ID
# selected.effective_release_id; the RELEASE query aliases it selected.release_id.
active_closure = closure.replace('selected.release_id', 'selected.effective_release_id')
replace(r'''       )::text AS node_components \
  FROM selected \
''', r'''       )::text AS node_components, \
''' + active_closure + r'''  FROM selected \
''')
replace('          selected.graph_json, selected.wiring_hash";',
        '          selected.graph_json, selected.wiring_hash, selected.manifest";')
proposed = original[:start] + changed + original[end:]
(here / 'wiring_resolution.rs.proposed').write_text(proposed)
(here / 'proposal.patch').write_text(''.join(difflib.unified_diff(
    original.splitlines(keepends=True), proposed.splitlines(keepends=True),
    fromfile='a/' + str(relative), tofile='b/' + str(relative))))
(here / 'source-inputs.json').write_text(json.dumps({
    'source_commit': 'ab0467f415dfd6b31e70323ec6ca049d5d2a5298',
    'source_file': str(source),
    'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
    'proposal_sha256': hashlib.sha256((here / 'proposal.patch').read_bytes()).hexdigest(),
    'proposed_source_sha256': hashlib.sha256(proposed.encode()).hexdigest(),
    'scope': 'Only ACTIVE_WIRING_SQL is changed; no tests, decoders, grants, fixture operations or other SQL constants are changed.',
    'executed': 'Preparation only. No compiler, Cargo, PostgreSQL, OCI or live execution.',
}, indent=2) + '\n')
