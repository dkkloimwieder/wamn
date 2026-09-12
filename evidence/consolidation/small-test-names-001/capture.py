from pathlib import Path
import hashlib
import json
import os
import subprocess
import time

root = Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-wiring-20260912')
out = Path('/home/kaalin/dev/wamn/evidence/consolidation/small-test-names-001')
out.mkdir(parents=True, exist_ok=False)
paths = ['crates/authoring/model/tests/contract.rs', 'crates/control/provision/tests/restore.rs', 'crates/identity/platform/src/lib.rs', 'tests/integration/src/cdc_reader_process.rs']

def source():
    return {
        'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
        'files': {name: {'sha256': hashlib.sha256((root / name).read_bytes()).hexdigest(),
                         'mode': oct((root / name).stat().st_mode & 0o777)} for name in paths},
        'status': subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=root, text=True),
    }

before = source()
(out / 'source-before.json').write_text(json.dumps(before, indent=2) + '\n')
(out / 'source.diff').write_bytes(subprocess.check_output(['git', 'diff'], cwd=root))
(out / 'capture.py').write_bytes(Path(__file__).read_bytes())
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'OTEL_')) and key not in {'DB_URL', 'DATABASE_URL'}}
env.update(RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2', CARGO_TARGET_DIR=str(root / 'target'), KUBECONFIG='/dev/null')
commands = [
    ['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p', 'wamn-authoring-model', '--test', 'contract', 'kinds_and_operation_pairing_are_exact', '--', '--nocapture', '--test-threads=1'],
    ['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p', 'wamn-integration-tests', '--lib', 'cdc_reader_process::tests::', '--', '--nocapture', '--test-threads=1'],
]
results = []
for index, argv in enumerate(commands, 1):
    start = time.monotonic()
    with (out / f'command-{index}.stdout').open('wb') as stdout, (out / f'command-{index}.stderr').open('wb') as stderr:
        run = subprocess.run(argv, cwd=root, env=env, stdout=stdout, stderr=stderr)
    row = {'argv': argv, 'exit_code': run.returncode, 'elapsed_seconds': time.monotonic() - start}
    results.append(row)
    (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(f'Command {index}: exit {run.returncode}, {row["elapsed_seconds"]:.3f} seconds', flush=True)
    if run.returncode:
        break
after = source()
(out / 'source-after.json').write_text(json.dumps(after, indent=2) + '\n')
assert after == before, 'Source changed during the run'
print('Source bytes, modes, and HEAD remained unchanged.', flush=True)
raise SystemExit(next((row['exit_code'] for row in results if row['exit_code']), 0))
