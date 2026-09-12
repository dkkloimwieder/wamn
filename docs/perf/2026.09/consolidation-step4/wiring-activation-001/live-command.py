import json
import os
from pathlib import Path
import subprocess
import sys
import time
from urllib.parse import quote

mode = sys.argv[1]
name = sys.argv[2] if len(sys.argv) > 2 else mode
out = Path('/tmp/consolidation-sql-wiring-tests-20260912')
env = os.environ.copy()
url = 'postgresql://{}:{}@{}:{}/{}'.format(
    quote(env['PGUSER']), quote(env['PGPASSWORD']), env['PGHOST'],
    env['PGPORT'], env['PGDATABASE'],
)
env['WAMN_CTL_PG_URL'] = url
env['WAMN_CATALOG_PG_URL'] = url
env['CARGO_BUILD_JOBS'] = '2'
env['CARGO_TARGET_DIR'] = '/home/kaalin/.cache/wamn-lanes/consolidation-sql-wiring-20260912/target'
commands = {
    'catalog-live': ['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p',
                     'wamn-catalog', '--test', 'wiring_activation_live', '--',
                     '--ignored', '--nocapture', '--test-threads=1'],
    'promotion-live': ['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p',
                       'wamn-ctl', '--lib', 'promote::', '--',
                       '--include-ignored', '--nocapture', '--test-threads=1'],
}
subprocess.run(['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-c',
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN CREATE ROLE postgres NOLOGIN SUPERUSER; END IF; END $$;"],
               env=env, check=True)
start = time.monotonic()
with (out / f'{name}.log').open('wb') as log:
    result = subprocess.run(commands[mode], env=env, stdout=log, stderr=subprocess.STDOUT)
record = {'command': commands[mode], 'exit_code': result.returncode,
          'seconds': time.monotonic() - start,
          'postgres_version': env['PGVERSION'],
          'cluster_configuration': env['PG_CLUSTER_CONF_ROOT']}
(out / f'{name}.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record), flush=True)
sys.exit(result.returncode)
