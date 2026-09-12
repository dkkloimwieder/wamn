#!/usr/bin/env python3
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

REPO = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
OUT = Path(__file__).resolve().parent
FILES = ['services/host/src/host.rs', 'tools/receiving-cluster-journey-run']


def git(*args):
    return subprocess.check_output(['git', *args], cwd=REPO, text=True)


source = git('rev-parse', 'HEAD').strip()
assert source == 'b8e9881dc8afb94971d6f63bdaa844ea9f29c045'
hashes = {name: hashlib.sha256((REPO / name).read_bytes()).hexdigest() for name in FILES}
(OUT / 'source.patch').write_text(git('diff', '--', *FILES))
(OUT / 'source-status.txt').write_text(git('status', '--porcelain'))
env = {k: v for k, v in os.environ.items()
       if not k.startswith(('WAMN_', 'WASH_', 'OTEL_', 'PG'))
       and k not in ('CARGO_TARGET_DIR', 'DATABASE_URL', 'DB_URL')}
env.update(CARGO_BUILD_JOBS='4', RUSTC_WRAPPER='')
commands = [
    ['cargo', 'test', '-p', 'wamn-host', '--bin', 'wamn-host', '--locked', '--offline',
     'host::tests::', '--', '--nocapture', '--test-threads=1'],
    ['cargo', 'clippy', '-p', 'wamn-host', '--all-targets', '--locked', '--offline'],
]
receipt = dict(source=source, source_files=hashes, dirty_source=True,
               started_unix_ns=time.time_ns(), commands=[],
               scope='Host unit tests, including an actual native probe task termination through the production failure translation and cleanup.',
               limits='This is an in-process listener test, not a WAMN subprocess failure or a deployed recovery proof.')
exit_code = 0
for index, argv in enumerate(commands):
    with (OUT / f'command-{index}.log').open('wb') as log:
        started = time.time_ns()
        result = subprocess.run(argv, cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT)
    receipt['commands'].append(dict(argv=argv, exit_code=result.returncode,
                                     started_unix_ns=started, finished_unix_ns=time.time_ns()))
    (OUT / 'summary.json').write_text(json.dumps(receipt, indent=2) + '\n')
    if result.returncode:
        exit_code = result.returncode
        break
receipt.update(finished_unix_ns=time.time_ns(), exit_code=exit_code,
               source_after=git('rev-parse', 'HEAD').strip(),
               source_files_unchanged=all(hashlib.sha256((REPO / n).read_bytes()).hexdigest() == h
                                          for n, h in hashes.items()))
assert receipt['source_after'] == source and receipt['source_files_unchanged']
(OUT / 'summary.json').write_text(json.dumps(receipt, indent=2) + '\n')
raise SystemExit(exit_code)
