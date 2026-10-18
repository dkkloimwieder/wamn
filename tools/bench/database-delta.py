"""Subtract PostgreSQL counters at the boundaries of one timed run."""
import json
from pathlib import Path
import sys

work, call = Path(sys.argv[1]), sys.argv[2]
run = work / f'{call}.json'
if run.exists():
    samples = json.loads(run.read_text())['postgres']
    before, after = (samples[point]['cluster'] for point in ('before', 'after'))
else:
    before, after = (json.loads((work / f'{call}.database-{point}.json').read_text())
                     for point in ('before', 'after'))


def delta(earlier, later):
    if earlier['stats_reset'] != later['stats_reset']:
        raise SystemExit('PostgreSQL counters reset during the run')
    return {key: value - earlier[key] for key, value in later.items()
            if isinstance(value, (int, float)) and isinstance(earlier.get(key), (int, float))}


io_key = lambda row: (row['backend_type'], row['object'], row['context'])
io_before = {io_key(row): row for row in before['io'] or []}
result = {name: delta(before[name], after[name]) for name in ('wal', 'checkpointer')}
result['io'] = [{**{key: row[key] for key in ('backend_type', 'object', 'context')},
                 **delta(io_before[io_key(row)], row)} for row in after['io'] or []]
(work / f'{call}.database-delta.json').write_text(json.dumps(result, indent=2) + '\n')
