#!/usr/bin/env python3
"""Syntax and local executable-stub controls; fake sockets never bind a port."""
import ast
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
STUB = r'''#!/usr/bin/env python3
import json,os,pathlib,sys,signal,time
root=pathlib.Path(os.environ['VIRT_STUB_ROOT']);mode=os.environ['VIRT_STUB_MODE'];tool=pathlib.Path(sys.argv[0]).name;args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as out:out.write(json.dumps({'tool':tool,'argv':args,'cwd':os.getcwd()})+'\n')
assert not any('postgresql://' in arg for arg in args)
repo=pathlib.Path.cwd(); raw=repo/'components/target/wasm32-wasip2/release'; virt=repo/'components/target/virtualized/std-empty-environment'
if tool=='git':
 if args[-2:]==['rev-parse','HEAD']:print('a'*40)
 elif args[-3:]==['status','--porcelain=v1','--untracked-files=normal']:
  if mode=='dirty':print(' M source.rs')
 else:raise AssertionError(args)
elif tool=='build-components':
 assert args in [['proof'],['m1']]
 assert 'CARGO_TARGET_DIR' not in os.environ and os.environ['RUSTC_WRAPPER']==''
 if args==['m1'] and mode=='restore-failure':sys.exit(29)
 raw.mkdir(parents=True,exist_ok=True);virt.mkdir(parents=True,exist_ok=True)
 for name in ['std_virtualization_probe.wasm','http_route.wasm']:(raw/name).write_bytes(args[0].encode()+name.encode())
 (virt/'receiving.wasm').write_bytes(args[0].encode()+b'receiving')
elif tool=='cargo':
 assert 'CARGO_TARGET_DIR' not in os.environ
 if args[0]=='run':
  assert args[args.index('--input')+1]=='components/target/wasm32-wasip2/release/std_virtualization_probe.wasm'
  assert pathlib.Path(args[args.index('--output')+1])==virt/'std_virtualization_probe.wasm'
  (virt/'std_virtualization_probe.wasm').write_bytes(b'virtualized probe');sys.exit(0)
 assert args[0]=='test' and '--include-ignored' in args and '--exact' in args
 name=args[args.index('--lib')+1];live='hides_the_sentinel' in name
 assert pathlib.Path(os.environ['WAMN_STD_VIRTUALIZATION_COMPONENT_WASM'])==virt/'std_virtualization_probe.wasm'
 if live:
  assert os.environ['WAMN_STD_VIRTUALIZATION_FLOW_HTTP_WASM']==str(raw/'http_route.wasm')
  assert os.environ['WAMN_STD_VIRTUALIZATION_PG_URL']=='postgresql://postgres:probe@127.0.0.1:12001/postgres'
  assert os.environ['WAMN_STD_VIRTUALIZATION_ARTIFACT_BASE']=='127.0.0.1:12002/wamn/std-proof'
  print('private URI '+os.environ['WAMN_STD_VIRTUALIZATION_PG_URL'])
 if live and mode=='interrupted':
  os.kill(os.getppid(),signal.SIGINT);time.sleep(0.2);sys.exit(130)
 print('private password=fixture-secret-value')
 bad=live and mode=='live-failure';ignored=live and mode=='ignored';zero=live and mode=='zero-tests'
 passed=0 if bad or ignored or zero else 1;failed=int(bad)
 print('running '+str(0 if zero else 1)+' tests')
 print(f'test {name} ... '+('FAILED' if bad else 'ignored' if ignored else 'ok'))
 print(f'test result: {"FAILED" if bad else "ok"}. {passed} passed; {failed} failed; {int(ignored)} ignored; 0 measured; 0 filtered out; finished in 0.01s')
 sys.exit(101 if bad else 0)
elif tool=='psql':
 assert args[-1]=='SHOW server_version_num' and os.environ['PGPASSWORD']=='probe' and os.environ['PGPORT']=='12001';print('180006')
elif tool=='docker':
 state=root/'owned'; project_file=root/'project'
 if args[0]=='compose':
  assert args[1]=='-p' and args[3]=='-f' and args[4]=='test-support/infrastructure/std-virtualization.compose.yaml'
  project=args[2];assert project.startswith('wamn-cutover-std-')
  assert not any(key.startswith('COMPOSE_') for key in os.environ)
  if args[5]=='up':
   assert args[6:]==['--detach','--wait','--wait-timeout','60','postgres','registry']
   assert os.environ['WAMN_STD_VIRT_PG_PORT']=='12001' and os.environ['WAMN_STD_VIRT_REGISTRY_PORT']=='12002'
   assert (root/'ports-released').read_text()=='2'
   project_file.write_text(project);state.write_text('owned')
   if mode=='setup-failure':sys.exit(28)
  elif args[5]=='down':
   assert args[6:]==['--volumes'] and project==project_file.read_text()
   if mode=='cleanup-failure':sys.exit(27)
   state.unlink(missing_ok=True)
  else:raise AssertionError(args)
 elif args[0]=='ps':
  assert args[:3]==['ps','--all','--quiet'] and args[-2]=='--filter'
  project=args[-1].split('=',2)[-1]
  if project_file.exists():assert project==project_file.read_text()
  if state.exists() or mode=='collision':print('1'*64+'\n'+'2'*64)
 elif args[0]=='inspect':
  assert args[:3]==['inspect','--format','{{json .Config.Labels}}']
  print(json.dumps({'com.docker.compose.project':project_file.read_text(),'com.docker.compose.service':'postgres' if args[3]=='1'*64 else 'registry'}))
 elif args[0] in ['network','volume']:
  assert args[1:4]==['ls','--quiet','--filter']
  if state.exists():print('remaining-resource')
 else:raise AssertionError(args)
else:raise AssertionError(tool)
'''
DRIVER = r'''import importlib.util,os,pathlib,sys
here=pathlib.Path(sys.argv[1]);sys.path.insert(0,str(here));spec=importlib.util.spec_from_file_location('virtualization',here/'run-virtualization.py');module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
class FakeSocket:
 count=0
 released=0
 def __init__(self):
  type(self).count+=1;self.port=12000+type(self).count;self.closed=False
 def bind(self,address):assert address==('127.0.0.1',0)
 def getsockname(self):return ('127.0.0.1',self.port)
 def close(self):
  if not self.closed:
   self.closed=True;type(self).released+=1;pathlib.Path(os.environ['VIRT_STUB_ROOT'],'ports-released').write_text(str(type(self).released))
module.socket.socket=FakeSocket
sys.argv=[str(here/'run-virtualization.py')]+sys.argv[2:]
raise SystemExit(module.main())
'''


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    for name in ('run-virtualization.py','proof_support.py','validate-offline.py'):
        ast.parse((HERE/name).read_text())
    controls=[]
    with tempfile.TemporaryDirectory(prefix='wamn virtualization offline ') as temp:
        base=Path(temp); bindir=base/'bin';bindir.mkdir()
        for name in ('git','cargo','docker','psql'):
            file=bindir/name;file.write_text(STUB);file.chmod(0o700)
        driver=base/'driver.py';driver.write_text(DRIVER)
        for mode,expected_exit,expected_tests,expected_down,expected_restore in [('success',0,2,1,1),('setup-failure',1,1,1,1),('live-failure',1,2,1,1),('ignored',1,2,1,1),('zero-tests',1,2,1,1),('collision',1,1,0,1),('restore-failure',1,2,1,1),('cleanup-failure',1,2,1,1),('interrupted',1,2,1,1),('dirty',1,0,0,0)]:
            location=base/mode;location.mkdir();repo=location/'source with spaces';(repo/'tools').mkdir(parents=True)
            build=repo/'tools/build-components';build.write_text(STUB);build.chmod(0o700)
            evidence=location/'evidence with spaces'
            env=os.environ|{'PATH':str(bindir)+os.pathsep+os.environ['PATH'],'VIRT_STUB_ROOT':str(location),'VIRT_STUB_MODE':mode,'COMPOSE_PROFILES':'must-not-inherit','CARGO_TARGET_DIR':'must-not-inherit','DATABASE_URL':'must-not-inherit'}
            process=subprocess.run(['python3',str(driver),str(HERE),'--repo',str(repo),'--source','a'*40,'--evidence-dir',str(evidence)],env=env,text=True,capture_output=True,timeout=60)
            assert process.returncode==expected_exit,(mode,process.stdout,process.stderr)
            summary=json.loads((evidence/'summary.json').read_text());assert sum(t['invoked'] for t in summary['tests'])==expected_tests,(mode,summary)
            calls=[json.loads(line) for line in (location/'calls.jsonl').read_text().splitlines()]
            down=[c for c in calls if c['tool']=='docker' and c['argv'][0]=='compose' and c['argv'][5]=='down'];restore=[c for c in calls if c['tool']=='build-components' and c['argv']==['m1']]
            assert len(down)==expected_down and len(restore)==expected_restore,(mode,calls)
            if mode=='success':assert summary['proof_passed'] and summary['m1_restore']['passed'] and not summary['m1_restore']['required']
            if mode=='restore-failure':assert summary['proof_passed'] and summary['m1_restore']['required']
            if mode=='cleanup-failure':assert summary['proof_passed'] and not summary['compose']['cleanup_passed']
            if mode=='ignored':assert summary['tests'][1]['classification']=='invoked_but_ignored_or_self_skipped'
            if mode=='zero-tests':assert summary['tests'][1]['classification']=='invoked_without_executed_test'
            if mode=='interrupted':assert summary['tests'][1]['invoked'] and not summary['tests'][1]['passed'] and summary['compose']['cleanup_passed']
            public='\n'.join(p.read_text() for p in evidence.iterdir() if p.is_file())+process.stdout+process.stderr
            assert 'postgresql://postgres:probe' not in public and 'fixture-secret-value' not in public
            for path in (evidence/'private').iterdir():
                if path.is_file():assert stat.S_IMODE(path.stat().st_mode)==0o600
            controls.append({'name':mode,'passed':True,'invoked_tests':expected_tests,'owned_cleanup_calls':expected_down,'m1_restore_calls':expected_restore,'exit':process.returncode})
    receipt={'schema':'wamn-cutover-virtualization-offline-validation/v1','controls':controls,'file_sha256':{name:sha(HERE/name) for name in ('run-virtualization.py','proof_support.py','selection.json','validate-offline.py')},'scope':'Python syntax and local executable stubs only. Socket objects were fake; no ports were bound and no real Git, Cargo, Docker, psql, build-components, or network call ran.'}
    (HERE/'offline-validation.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt,indent=2))


if __name__=='__main__':main()
