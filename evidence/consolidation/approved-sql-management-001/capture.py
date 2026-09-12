import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.parse

root = Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912')
out = Path(__file__).resolve().parent

def capture(argv, name, env):
    start = time.monotonic()
    stamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with (out / (name + '.log')).open('wb') as log:
        os.chmod(log.name, 0o600)
        result = subprocess.run(argv, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
    record = dict(argv=argv, cwd=str(root), started=stamp, elapsed_seconds=time.monotonic()-start, exit=result.returncode)
    (out / (name + '.json')).write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    return result.returncode

env = {k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_', 'PG', 'OTEL_')) or k in ('DATABASE_URL', 'DB_URL', 'CARGO_TARGET_DIR'))}
env.update(RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2', SQLX_OFFLINE='true', KUBECONFIG='/dev/null')
if len(sys.argv) > 1:
    url = urllib.parse.urlunsplit(('postgresql', urllib.parse.quote(os.environ['PGUSER'], safe='') + ':' + urllib.parse.quote(os.environ['PGPASSWORD'], safe='') + '@127.0.0.1:' + os.environ['PGPORT'], '/postgres', '', ''))
    env['WAMN_MANAGEMENT_ADMITTER_PG18_URL'] = url
    pg = dict(version=subprocess.check_output(['psql', '-X', '-Atqc', 'SHOW server_version'], text=True).strip(), configuration_root=os.environ['PG_CLUSTER_CONF_ROOT'], port=os.environ['PGPORT'])
    (out / 'postgres.json').write_text(json.dumps(pg, indent=2) + '\n')
    subprocess.run(['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-q'], input="DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN CREATE ROLE postgres NOLOGIN SUPERUSER; END IF; END $$;", text=True, check=True)
    raise SystemExit(capture(['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p', 'wamn-ctl', '--lib', '--test', 'management_admitter_generation_live', 'management_', '--', '--include-ignored', '--nocapture', '--test-threads=1'], 'management-callers', env))

head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
tree = subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], cwd=root, text=True).strip()
assert not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=root, text=True)
(out / 'source.json').write_text(json.dumps(dict(commit=head, tree=tree, environment={k:env[k] for k in ('RUSTC_WRAPPER', 'CARGO_BUILD_JOBS', 'SQLX_OFFLINE', 'KUBECONFIG')}), indent=2) + '\n')
unit = capture(['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p', 'wamn-control-provision', '--lib', 'management_', '--', '--nocapture'], 'management-builders', env)
live = capture(['pg_virtualenv', '-t', '-v', '18', sys.executable, str(__file__), 'inside'], 'postgres-controller', env) if unit == 0 else None
pg_path = out / 'postgres.json'
cleaned = not Path(json.loads(pg_path.read_text())['configuration_root']).exists() if pg_path.exists() else None
unchanged = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip() == head and not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=root, text=True)
(out / 'result.json').write_text(json.dumps(dict(unit_exit=unit, live_exit=live, temporary_cluster_removed=cleaned, source_unchanged=unchanged), indent=2) + '\n')
raise SystemExit(unit or live or not unchanged or (cleaned is False))
