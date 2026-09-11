import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

lane = Path('/home/kaalin/.cache/wamn-lanes/native-b-adoption-20260910')
evidence = Path('/home/kaalin/dev/wamn/docs/perf/2026.09/native-b-adoption') / sys.argv[1]
command = sys.argv[2:]
evidence.mkdir(parents=True, exist_ok=False)
def git(*args):
    return subprocess.check_output(['git', *args], cwd=lane)
paths = git('ls-files', '--cached', '--others', '--exclude-standard', '-z', '*.rs', '*Cargo.toml', '*Cargo.lock', 'rust-toolchain.toml', 'tools/journey-trace.sh', 'tools/journey-trace-proof').decode().split('\0')
identities = {p: hashlib.sha256((lane / p).read_bytes()).hexdigest() for p in paths if p and (lane / p).is_file()}
(evidence / 'source.json').write_text(json.dumps({'head': git('rev-parse', 'HEAD').decode().strip(), 'sha256': identities}, indent=2)+'\n')
(evidence / 'source.patch').write_bytes(git('diff', '--', '*.rs', '*Cargo.toml', '*Cargo.lock', 'tools/journey-trace.sh', 'tools/journey-trace-proof'))
(evidence / 'command.json').write_text(json.dumps(command)+'\n')
start = time.monotonic()
with (evidence / 'output.log').open('wb') as output:
    result = subprocess.run(command, cwd=lane, stdout=output, stderr=subprocess.STDOUT)
changed = [p for p,h in identities.items() if not (lane / p).is_file() or hashlib.sha256((lane/p).read_bytes()).hexdigest()!=h]
(evidence / 'result.json').write_text(json.dumps({'command': command, 'exit_code': result.returncode, 'elapsed_seconds': time.monotonic()-start, 'changed_during_run': changed}, indent=2)+'\n')
print(json.dumps({'evidence':str(evidence), 'exit_code':result.returncode, 'changed_during_run':changed}), flush=True)
print((evidence/'output.log').read_text()[-16000:])
sys.exit(result.returncode)
