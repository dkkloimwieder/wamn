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
root = Path('/home/kaalin/.cache/wamn-lanes/receiving-postcommit-final-20260910')
out = Path(__file__).resolve().parent
source = '4cc0e99c817d1fecdea1a29ab8c8b6bb94261aad'
cases = {
    'production-claim-durable-002': ('WAMN_DURABLE_TIER_PG_URL', 'production_claim_durable_live'),
}

def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def safe(text, secrets=()):
    for value in sorted((v for v in secrets if v), key=len, reverse=True):
        text = text.replace(value, '<redacted>')
    text = re.sub(r"(?i)(PASSWORD\s+)'(?:[^']|'')*'", r"\1'<redacted>'", text)
    return re.sub(r'(postgres(?:ql)?://[^:\s]+:)[^@\s]+(@)', r'\1<redacted>\2', text)

def capture(argv, name, env, directory, secrets=()):
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    clock = time.monotonic()
    process = subprocess.run(argv, cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    log = safe(process.stdout.decode('utf-8', errors='replace'), secrets)
    (directory / (name + '.log')).write_text(log)
    record = dict(argv=argv, cwd=str(root), started=started, ended=datetime.datetime.now(datetime.timezone.utc).isoformat(), elapsed_seconds=time.monotonic()-clock, exit=process.returncode)
    save(directory / (name + '.json'), record)
    print(json.dumps(record), flush=True)
    return process.returncode

inside = sys.argv[1] == '--inside'
case, binary = sys.argv[2:4] if inside else sys.argv[1:3]
key, test = cases[case]
directory = out / case
env = {k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_', 'PG', 'OTEL_')) or k in ('DATABASE_URL', 'DB_URL', 'CARGO_TARGET_DIR'))}
if inside:
    url = urllib.parse.urlunsplit(('postgresql', urllib.parse.quote(os.environ['PGUSER'], safe='') + ':' + urllib.parse.quote(os.environ['PGPASSWORD'], safe='') + '@127.0.0.1:' + os.environ['PGPORT'], '/postgres', '', ''))
    env[key] = url
    info = json.loads(subprocess.check_output(['psql', '-X', '-Atqc', "SELECT json_build_object('server_version',current_setting('server_version'),'server_version_num',current_setting('server_version_num'),'database',current_database(),'configuration_root',current_setting('config_file'),'data_directory',current_setting('data_directory'),'port',current_setting('port'))"], text=True))
    assert 180000 <= int(info['server_version_num']) < 190000
    info['configuration_root'] = os.environ['PG_CLUSTER_CONF_ROOT']
    info['server_pid'] = int((Path(info['data_directory']) / 'postmaster.pid').read_text().splitlines()[0])
    save(directory / 'postgres.json', info)
    argv = [binary, test, '--exact', '--ignored', '--nocapture']
    raise SystemExit(capture(argv, 'test', env, directory, [url, os.environ['PGPASSWORD']]))

assert Path(binary).is_file()
head = subprocess.check_output(['git','rev-parse','HEAD'], cwd=root, text=True).strip()
assert head == source
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'], cwd=root, text=True)
directory.mkdir(mode=0o700)
files = {}
for relative in ['crates/platform/runtime/tests/common/mod.rs','crates/platform/runtime/tests/production_claim_live.rs','crates/platform/runtime/tests/production_claim_durable_live.rs']:
    path = root / relative
    files[relative] = {'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'mode':oct(path.stat().st_mode & 0o777)}
metadata = {'commit':head,'tree':subprocess.check_output(['git','rev-parse','HEAD^{tree}'],cwd=root,text=True).strip(),'files':files,'binary':binary,'binary_sha256':hashlib.sha256(Path(binary).read_bytes()).hexdigest(),'binary_bytes':Path(binary).stat().st_size,'private_environment_key':key}
save(directory / 'source.json',metadata)
argv = ['pg_virtualenv','-t','-v','18','-o','log_min_error_statement=panic',sys.executable,str(Path(__file__).resolve()),'--inside',case,binary]
exit_code = capture(argv,'controller',env,directory)
pg_file = directory / 'postgres.json'
cleanup = None
if pg_file.exists():
    pg = json.loads(pg_file.read_text())
    with socket.socket() as client:
        client.settimeout(1)
        port_closed = client.connect_ex(('127.0.0.1', int(pg['port']))) != 0
    cleanup = {'configuration_removed':not Path(pg['configuration_root']).exists(),'data_directory_removed':not Path(pg['data_directory']).exists(),'server_pid_absent':not Path('/proc',str(pg['server_pid'])).exists(),'port_closed':port_closed}
unchanged = subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip() == head and not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=root,text=True)
unchanged = bool(unchanged and all(hashlib.sha256((root / path).read_bytes()).hexdigest() == facts['sha256'] for path,facts in files.items()))
result = {'case':case,'compiled_source':source,'exit':exit_code,'cleanup':cleanup,'source_unchanged':unchanged,'scope':'One existing test case on its own temporary PostgreSQL 18 server; no production automation admission claim.'}
save(directory/'result.json',result)
print(json.dumps(result),flush=True)
raise SystemExit(exit_code or not unchanged or not cleanup or not all(cleanup.values()))
