from pathlib import Path
import datetime,hashlib,json,os,re,subprocess,time
w=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912')
o=Path('/home/kaalin/dev/wamn/evidence/consolidation/final-internal-names-20260912-001');o.mkdir(mode=0o700)
m=json.loads(Path('/tmp/consolidation-final-internal-names-map.json').read_text())
paths=list(m['rust'])+list(m['shell'])+['tools/identity-jwks-journey-run','apps/wamn_receiving/tests/operator_pty.py']
base=subprocess.check_output(['git','rev-parse','HEAD'],cwd=w,text=True).strip()
assert base=='a2451f069443a45f996a8724b1180006daab2dd1'
snapshot={p:{'sha256':hashlib.sha256((w/p).read_bytes()).hexdigest(),'mode':oct((w/p).stat().st_mode&0o777)} for p in paths}
(o/'source.json').write_text(json.dumps({'head':base,'files':snapshot},indent=2)+'\n')
pat=re.compile(r'(?P<literal>r(?P<h>#{0,255})".*?"(?P=h)|"(?:\\.|[^"\\])*")|(?P<comment>//[^\n]*|/\*.*?\*/)|(?P<identifier>\b[A-Za-z_][A-Za-z_0-9]*\b)|(?P<other>[^\s])',re.S)
def tokens(s):return [(x.lastgroup,x[0]) for x in pat.finditer(s) if x.lastgroup!='comment']
checks=[]
for p,mapping in m['rust'].items():
 old=subprocess.check_output(['git','show',base+':'+p],cwd=w,text=True);new=(w/p).read_text();a=tokens(old);b=tokens(new);assert len(a)==len(b)
 literals=[];exceptions=[]
 for (k,x),(l,y) in zip(a,b):
  assert k==l
  if k=='identifier':assert y==mapping.get(x,x),(p,x,y)
  elif k=='literal' and x!=y:
   assert p.endswith('authenticated.rs') and x=='"{receipt}"' and y=='"{result_line}"',(p,x,y)
   exceptions.append({'before':x,'after':y,'rendered_value':'the same stdout line'})
  else:
   assert x==y,(p,x,y)
   if k=='literal':literals.append(x)
 checks.append({'path':p,'code_unchanged_except_names':True,'unchanged_string_literals':len(literals),'string_literals_sha256':hashlib.sha256('\0'.join(literals).encode()).hexdigest(),'format_capture_changes':exceptions})
for p,mapping in m['shell'].items():
 old=subprocess.check_output(['git','show',base+':'+p],cwd=w,text=True);new=(w/p).read_text()
 for before,after in mapping.items():new=re.sub(r'\b'+after+r'\b',before,new)
 assert old==new,p
 checks.append({'path':p,'exact_bytes_except_declared_variable_names':True})
p='tools/identity-jwks-journey-run';old=subprocess.check_output(['git','show',base+':'+p],cwd=w,text=True);new=(w/p).read_text()
for before,after in [('--arg receipt ','--arg result_line '),('log_contains:$receipt','log_contains:$result_line'),('local receipt=$1','local result_name=$1'),('$evidence_dir/$receipt.json','$evidence_dir/$result_name.json')]:new=new.replace(after,before)
assert old==new,p;checks.append({'path':p,'exact_bytes_except_declared_variable_names':True})
p='apps/wamn_receiving/tests/operator_pty.py';old=subprocess.check_output(['git','show',base+':'+p],cwd=w,text=True);new=(w/p).read_text();assert old==new.replace('assert_operator','prove');checks.append({'path':p,'exact_bytes_except_function_name':True})
(o/'source-comparison.json').write_text(json.dumps({'base':base,'files':checks,'guest_and_manifests_changed':False},indent=2)+'\n')
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
records=[]
def run(argv,name):
 t=time.monotonic();start=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with (o/name).open('xb') as f:
  os.chmod(f.name,0o600);r=subprocess.run(argv,cwd=w,env=env,stdout=f,stderr=subprocess.STDOUT)
 record={'argv':argv,'cwd':str(w),'started':start,'elapsed_seconds':time.monotonic()-t,'exit':r.returncode,'log':name,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}};records.append(record)
 (o/'commands.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps(record),flush=True);assert r.returncode==0
try:
 run(['bash','-n','tools/kind-gate-image-remove'],'kind-shell-syntax.log')
 run(['bash','-n','tools/identity-jwks-journey-run'],'identity-shell-syntax.log')
 run(['python3','-c','from pathlib import Path; p=Path("apps/wamn_receiving/tests/operator_pty.py"); compile(p.read_text(), str(p), "exec")'],'python-syntax.log')
 run(['rustfmt','+1.98.0','--edition','2024','--emit','stdout','tools/probes/ctc8-14-wasi-http/host/src/main.rs'],'native-probe-parse.log')
 run(['cargo','+1.98.0','test','--locked','--offline','-p','wamn-receiving-tests','-p','wamn-integration-tests','-p','wamn-execution-host','-p','wamn-conformance-tests','--lib','--test','kind_gate_image_remove','--no-run','--message-format=json-render-diagnostics'],'caller-build.log')
 artifacts=[]
 for line in (o/'caller-build.log').read_text().splitlines():
  try:r=json.loads(line)
  except ValueError:continue
  if r.get('reason')=='compiler-artifact' and r.get('executable'):artifacts.append({'name':r['target']['name'],'kind':r['target']['kind'],'path':r['executable']})
 (o/'artifacts.json').write_text(json.dumps(artifacts,indent=2)+'\n')
 def binary(name):
  matches=[r['path'] for r in artifacts if r['name']==name];assert len(matches)==1,(name,matches);return matches[0]
 run([binary('wamn_integration_tests'),'identity_session_test::tests::','--nocapture'],'identity-session-tests.log')
 run([binary('kind_gate_image_remove'),'--nocapture'],'kind-image-tests.log')
finally:
 (o/'source-after.json').write_text(json.dumps({'unchanged':all(hashlib.sha256((w/p).read_bytes()).hexdigest()==r['sha256'] and oct((w/p).stat().st_mode&0o777)==r['mode'] for p,r in snapshot.items()),'head_unchanged':subprocess.check_output(['git','rev-parse','HEAD'],cwd=w,text=True).strip()==base},indent=2)+'\n')
