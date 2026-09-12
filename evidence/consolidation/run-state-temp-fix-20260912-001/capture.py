import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time
import urllib.parse

os.umask(0o077)
out = Path(__file__).resolve().parent
compiled = json.loads((out / 'compile.json').read_text())
root = Path(compiled['cwd'])
binary = Path(compiled['test_artifacts'][0]['executable'])
source = compiled['source']

def save(name, value):
    (out / name).write_text(json.dumps(value, indent=2) + '\n')

def safe(text, secrets=()):
    for value in sorted((v for v in secrets if v), key=len, reverse=True):
        text = text.replace(value, '<redacted>')
    text = re.sub(r"(?i)(PASSWORD\s+)'(?:[^']|'')*'", r"\1'<redacted>'", text)
    return re.sub(r'(postgres(?:ql)?://[^:\s]+:)[^@\s]+(@)', r'\1<redacted>\2', text)

def capture(argv, name, env, secrets=()):
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    clock = time.monotonic()
    process = subprocess.run(argv, cwd=root, env=env, capture_output=True)
    stdout = safe(process.stdout.decode(errors='replace'), secrets)
    (out / (name + '.stdout')).write_text(stdout)
    (out / (name + '.stderr')).write_text(safe(process.stderr.decode(errors='replace'), secrets))
    save(name + '.json', dict(argv=argv, cwd=str(root), started=started, elapsed_seconds=time.monotonic()-clock, exit_code=process.returncode))
    return process.returncode, stdout

env = {k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_', 'PG', 'OTEL_')) or k in ('DATABASE_URL', 'DB_URL', 'CARGO_TARGET_DIR'))}
if '--inside' in sys.argv:
    info = json.loads(subprocess.check_output(['psql', '-X', '-Atqc', "SELECT json_build_object('server_version',current_setting('server_version'),'server_version_num',current_setting('server_version_num'),'database',current_database(),'data_directory',current_setting('data_directory'),'port',current_setting('port'))"], text=True))
    assert 180000 <= int(info['server_version_num']) < 190000
    info['configuration_root'] = os.environ['PG_CLUSTER_CONF_ROOT']
    info['server_pid'] = int((Path(info['data_directory']) / 'postmaster.pid').read_text().splitlines()[0])
    save('postgres.json', info)
    url = urllib.parse.urlunsplit(('postgresql', urllib.parse.quote(os.environ['PGUSER'], safe='') + ':' + urllib.parse.quote(os.environ['PGPASSWORD'], safe='') + '@127.0.0.1:' + os.environ['PGPORT'], '/' + info['database'], '', ''))
    private = [url, os.environ['PGPASSWORD']]
    revoke = "DO $$ BEGIN EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC',current_database()); END $$; SELECT NOT EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL aclexplode(COALESCE(d.datacl,acldefault('d',d.datdba))) acl WHERE d.datname=current_database() AND acl.grantee=0 AND acl.privilege_type='TEMPORARY');"
    code, text = capture(['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-Atqc', revoke], 'revoke-public-temp', os.environ.copy(), private)
    assert code == 0 and text.strip() == 't', 'PUBLIC TEMPORARY must be absent before the test'
    env['WAMN_RUN_STORE_PG_URL'] = url
    test_code, text = capture([str(binary), '--exact', 'run_state_live', '--ignored', '--nocapture'], 'test', env, private)
    query = "SELECT has_database_privilege('wamn_transitions_executor_login', current_database(), 'TEMPORARY');"
    check_code, privilege = capture(['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-Atqc', query], 'executor-temp', os.environ.copy(), private)
    privilege_absent = check_code == 0 and privilege.strip() == 'f'
    count = re.search(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; \d+ measured; (\d+) filtered out;', text)
    counts = tuple(map(int, count.groups())) if count else None
    save('observations.json', {'test_exit_code':test_code, 'reported_counts':counts, 'public_temporary_revoked_before_test':True, 'executor_temporary_absent_after_test':privilege_absent})
    raise SystemExit(test_code or (0 if privilege_absent and counts == (1,0,0,0) else 1))

assert compiled['exit_code'] == 0
assert hashlib.sha256(binary.read_bytes()).hexdigest() == compiled['test_artifacts'][0]['sha256']
assert subprocess.check_output(['git','rev-parse','HEAD'], cwd=root, text=True).strip() == source
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'], cwd=root, text=True)
argv = ['pg_virtualenv','-t','-v','18','-o','log_min_error_statement=panic',sys.executable,str(Path(__file__).resolve()),'--inside']
exit_code, _ = capture(argv,'controller',env)
cleanup = None
if (out / 'postgres.json').exists():
    pg = json.loads((out / 'postgres.json').read_text())
    with socket.socket() as client:
        client.settimeout(1)
        port_closed = client.connect_ex(('127.0.0.1', int(pg['port']))) != 0
    cleanup = {'configuration_removed':not Path(pg['configuration_root']).exists(),'data_directory_removed':not Path(pg['data_directory']).exists(),'server_pid_absent':not Path('/proc',str(pg['server_pid'])).exists(),'port_closed':port_closed}
unchanged = subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip() == source and not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=root,text=True)
result = {'source':source,'exit_code':exit_code,'cleanup':cleanup,'source_unchanged':unchanged}
save('result.json',result)
print(json.dumps(result),flush=True)
raise SystemExit(exit_code or not unchanged or not cleanup or not all(cleanup.values()))
