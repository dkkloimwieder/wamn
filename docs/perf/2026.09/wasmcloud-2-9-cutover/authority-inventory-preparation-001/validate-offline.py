#!/usr/bin/env python3
"""Exercise only local command stubs; never invoke real Git, Docker, psql, or Cargo."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
SOURCE = 'a' * 40
STUB = r'''#!/usr/bin/env python3
import json,os,pathlib,sys,time
name=pathlib.Path(sys.argv[0]).name; args=sys.argv[1:]; root=pathlib.Path(os.environ['AUTH_STUB_ROOT']); mode=os.environ['AUTH_STUB_MODE']
def record():
 with (root/'calls.jsonl').open('a') as out:out.write(json.dumps({'tool':name,'argv':args,'cwd':os.getcwd()})+'\n')
record()
assert not any('postgresql://' in a or 'postgres://' in a for a in args)
if name=='git':
 if args[-2:]==['rev-parse','HEAD']:print(('b' if mode=='wrong-source' else 'a')*40)
 elif args[-3:]==['status','--porcelain=v1','--untracked-files=normal']:
  if mode=='dirty-source':print(' M changed.rs')
 else:raise AssertionError(args)
elif name=='docker':
 if args[0]=='run':
  count=root/'count'; n=int(count.read_text())+1 if count.exists() else 1;count.write_text(str(n)); cid=f'{n:064x}'
  envfile=pathlib.Path(args[args.index('--env-file')+1]); cidfile=pathlib.Path(args[args.index('--cidfile')+1])
  assert envfile.stat().st_mode & 0o777==0o600
  secret=envfile.read_text().strip().split('=',1)[1]
  assert all(secret not in a for a in args)
  with (root/'secrets').open('a') as out:out.write(secret+'\n')
  assert args[args.index('--publish')+1]=='127.0.0.1::5432'
  if mode=='failed-setup':sys.exit(31)
  cidfile.write_text(cid+'\n')
  if mode=='failed-setup-after-cid':sys.exit(32)
  (root/(cid+'.owned')).write_text(str(cidfile))
  if 'scs-off-refusal' in args[args.index('--name')+1]:assert args[-2:]==['-c','standard_conforming_strings=off']
  else:assert args[-1]=='postgres:18'
  print(cid)
 elif args[0]=='port':
  assert len(args)==3 and len(args[1])==64 and args[2]=='5432/tcp';print('127.0.0.1:65431')
 elif args[0]=='inspect':
  assert args[:3]==['inspect','--format','{{.Image}}'];print('sha256:'+'d'*64)
 elif args[0]=='rm':
  assert args[:3]==['rm','--force','--volumes'] and len(args)==4
  cid=args[3];assert len(cid)==64
  if mode=='failed-cleanup' or (mode=='failed-matrix-cleanup' and cid==f'{2:064x}'):sys.exit(33)
  (root/(cid+'.owned')).unlink(missing_ok=True)
 else:raise AssertionError(args)
elif name=='psql':
 assert os.environ['PGHOST']=='127.0.0.1' and os.environ['PGPORT']=='65431' and os.environ['PGPASSWORD']
 assert all(os.environ['PGPASSWORD'] not in a for a in args)
 if args[-1]=='SHOW server_version_num':print('180006')
 else:
  assert args[:-1]==['--no-psqlrc','--quiet','--set','ON_ERROR_STOP=1','--tuples-only','--no-align','--file']
  assert pathlib.Path(args[-1]).is_file() and pathlib.Path(args[-1]).name=='capture-event-registration.sql'
  assert os.environ['PGDATABASE']=='wamn'
  if mode=='failed-capture':sys.exit(34)
  if mode=='invalid-capture':print('private '+os.environ['PGPASSWORD']);sys.exit(0)
  facts={'schema':'wamn-event-registration-write-capture/v1','database':'wamn','server_version_num':180006,'transaction_read_only':True,'relation':'catalog.event_registrations','relation_count':1,'owner':'postgres','rls_enabled':True,'rls_forced':True,'nonowner_table_writes':[],'nonowner_column_writes':[],'guest_select':True,'guest_table_update':False,'guest_any_column_update':False,'guest_tenant_id_update':False}
  if mode=='old-grant':facts['nonowner_column_writes']=[{'role':'wamn_app','operation':'update(tenant_id)'}];facts['guest_any_column_update']=True;facts['guest_tenant_id_update']=True
  if mode=='empty-relation':facts['relation_count']=0
  if mode=='unsafe-role':facts['nonowner_column_writes']=[{'role':'minted-secret-123','operation':'update(tenant_id)'}]
  if mode=='wrong-database':facts['database']='postgres'
  print(json.dumps(facts))
elif name=='cargo':
 assert 'WAMN_PG_URL' not in os.environ and 'DATABASE_URL' not in os.environ
 if args[0]=='build':
  assert os.environ['CARGO_TARGET_DIR']==str(pathlib.Path.cwd()/'components/target')
  out=pathlib.Path.cwd()/'components/target/wasm32-wasip2/debug/sqlx-command.wasm';out.parent.mkdir(parents=True,exist_ok=True);out.write_bytes(b'controlled artifact bytes')
  print('fake artifact build complete');sys.exit(0)
 assert args[0]=='test' and '--include-ignored' in args and '--nocapture' in args and '--test-threads=1' in args
 expected=1
 if '--test' in args:
  target=args[args.index('--test')+1];expected={'tenant_key_live':6,'deploy_sql_authority':4,'family_denial_matrix':20}.get(target,1)
 else:target=args[args.index('--lib')+1]
 envs=[(k,v) for k,v in os.environ.items() if k.startswith('WAMN_') and k.endswith('_URL')]
 assert envs
 for key,value in envs:
  assert value.startswith('postgresql://postgres:') and '@127.0.0.1:65431/' in value
  assert all(value not in a and os.environ['PGPASSWORD'] not in a for a in args)
  if key=='WAMN_BIND_CONNECTION_PROJECT_PG_URL':assert value.endswith('/bind_project')
  if key=='WAMN_BIND_CONNECTION_CONTROL_PG_URL':assert value.endswith('/bind_control')
 if target=='sqlx_transaction_live':assert pathlib.Path(os.environ['WAMN_SQLX_TRANSACTION_COMPONENT']).read_bytes()==b'controlled artifact bytes'
 print('private credential '+os.environ['PGPASSWORD'])
 print('postgresql://fixture:minted-secret-123@127.0.0.1:65431/owned')
 print('raw diagnostic password="minted-secret-123"')
 if mode=='zero-tests':expected=0
 print('running '+str(expected)+' tests')
 if mode=='self-skip':print('test fake_case ... skipping live case because fixture is absent')
 bad=mode=='failed-tests';passed=0 if bad else expected;failed=expected if bad else 0
 print('test fake_case ... '+('FAILED' if bad else 'ok'))
 print(f'test result: {"FAILED" if bad else "ok"}. {passed} passed; {failed} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')
 sys.exit(101 if bad else 0)
else:raise AssertionError(name)
'''


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    controls = []
    with tempfile.TemporaryDirectory(prefix='wamn authority stubs ') as temp:
        base = Path(temp)
        bin_dir = base/'bin'
        bin_dir.mkdir()
        for name in ('git','docker','psql','cargo'):
            path = bin_dir/name
            path.write_text(STUB)
            path.chmod(0o700)
        for mode, wanted_exit, wanted_cases, wanted_containers, wanted_cleanup in (
            ('success',0,2,2,2),
            ('failed-tests',1,2,2,2),
            ('failed-setup',1,1,1,0),
            ('failed-setup-after-cid',1,1,1,1),
            ('failed-cleanup',1,1,1,1),
            ('zero-tests',1,2,2,2),
            ('self-skip',1,2,2,2),
            ('failed-matrix-cleanup',1,2,2,2),
            ('failed-capture',1,2,2,2),
            ('invalid-capture',1,2,2,2),
            ('old-grant',1,2,2,2),
            ('empty-relation',1,2,2,2),
            ('unsafe-role',1,2,2,2),
            ('wrong-database',1,2,2,2),
            ('dirty-source',1,0,0,0),
            ('wrong-source',1,0,0,0),
        ):
            location = base/mode
            location.mkdir()
            repo = location/'repository with spaces'
            repo.mkdir()
            evidence = location/'evidence with spaces'
            env = os.environ | {'PATH':str(bin_dir)+os.pathsep+os.environ['PATH'],'AUTH_STUB_ROOT':str(location),'AUTH_STUB_MODE':mode,'WAMN_PG_URL':'must-not-inherit','DATABASE_URL':'must-not-inherit'}
            result = subprocess.run(['python3',str(HERE/'run-authority.py'),'--repo',str(repo),'--source',SOURCE,'--evidence-dir',str(evidence)],env=env,capture_output=True,text=True,timeout=60)
            assert result.returncode==wanted_exit,(mode,result.stdout,result.stderr)
            summary = json.loads((evidence/'summary.json').read_text())
            assert len(summary['cases'])==wanted_cases,(mode,summary)
            calls = [json.loads(line) for line in (location/'calls.jsonl').read_text().splitlines()]
            starts = [c for c in calls if c['tool']=='docker' and c['argv'][0]=='run']
            cleanup = [c for c in calls if c['tool']=='docker' and c['argv'][0]=='rm']
            assert len(starts)==wanted_containers and len(cleanup)==wanted_cleanup,(mode,calls)
            assert len({c['argv'][-1] for c in cleanup})==len(cleanup)
            if mode=='success':
                case_receipts = [json.loads((evidence/c['receipt']).read_text()) for c in summary['cases']]
                assert sum(c['test_summaries'][0]['passed'] for c in case_receipts)==24
                matrix=next(c for c in case_receipts if c['group']=='authority-denial-matrix')
                assert matrix['protected_write_capture']['verdict']=='pass'
                assert matrix['protected_write_capture']['fixture']['container_id']==matrix['cleanup']['owned_id']
                assert matrix['protected_write_capture']['observed']['database']=='wamn'
                assert len({c['group'] for c in case_receipts})==2
            capture_calls=[c for c in calls if c['tool']=='psql' and '--file' in c['argv']]
            if capture_calls:
                assert len(capture_calls)==1
                capture_index=calls.index(capture_calls[0])
                assert capture_index < calls.index(cleanup[-1])
                capture_receipt=json.loads((evidence/'event-registration-protected-write-capture.json').read_text())
                assert capture_receipt['verdict']==('pass' if mode in ('success','failed-matrix-cleanup') else 'fail')
            secrets=(location/'secrets').read_text().splitlines() if (location/'secrets').exists() else []
            public='\n'.join(p.read_text() for p in evidence.iterdir() if p.is_file())+result.stdout+result.stderr
            for secret in secrets+['minted-secret-123']:
                assert secret not in public,(mode,'secret reached public output')
            for path in (evidence/'private').rglob('*'):
                if path.is_file():assert stat.S_IMODE(path.stat().st_mode)==0o600,(mode,path)
            assert not list((evidence/'private').rglob('postgres.env'))
            assert all(c['cwd']==str(repo) for c in calls if c['tool']!='git')
            controls.append({'name':mode,'passed':True,'exit':result.returncode,'case_receipts':wanted_cases,'owned_start_calls':len(starts),'owned_cleanup_calls':len(cleanup)})
    output={'schema':'wamn-cutover-inventory-offline-validation/v1','controls':controls,'wrapper_sha256':sha(HERE/'run-authority.py'),'selection_sha256':sha(HERE/'selection.json'),'scope':'Local Git/Docker/psql/Cargo executable stubs only. No real services, network, builds, or source edits.'}
    (HERE/'offline-validation.json').write_text(json.dumps(output,indent=2)+'\n')
    print(json.dumps(output,indent=2))


if __name__=='__main__':
    main()
