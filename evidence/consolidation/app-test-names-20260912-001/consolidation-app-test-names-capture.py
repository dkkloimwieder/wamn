from pathlib import Path
import datetime,hashlib,json,os,re,subprocess,time
w=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912');o=Path('/home/kaalin/dev/wamn/evidence/consolidation/app-test-names-20260912-001')
paths=json.loads(Path('/tmp/consolidation-app-test-names-files.json').read_text())
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
source={p:{'sha256':hashlib.sha256((w/p).read_bytes()).hexdigest(),'mode':oct((w/p).stat().st_mode&0o777)} for p in paths}
(o/'source.json').write_text(json.dumps({'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=w,text=True).strip(),'files':source},indent=2)+'\n')
records=[]
def run(argv,name):
 t=time.monotonic();started=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with (o/name).open('xb') as f:
  os.chmod(f.name,0o600);r=subprocess.run(argv,cwd=w,env=env,stdout=f,stderr=subprocess.STDOUT)
 records.append({'argv':argv,'cwd':str(w),'started':started,'elapsed_seconds':time.monotonic()-t,'exit':r.returncode,'log':name,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}})
 (o/'commands.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps(records[-1]),flush=True);assert r.returncode==0
try:
 run(['cargo','+1.98.0','test','--locked','--offline','-p','wamn-receiving-tests','-p','wamn-integration-tests','--lib','--test','receiving_command_histories_live','--test','startup_burst_live','--no-run','--message-format=json-render-diagnostics'],'caller-build.log')
 artifacts=[];libs={}
 for line in (o/'caller-build.log').read_text().splitlines():
  try:r=json.loads(line)
  except ValueError:continue
  if r.get('reason')!='compiler-artifact':continue
  for f in r['filenames']:
   if f.endswith('.rlib'):libs[r['target']['name']]=f
  if r.get('executable'):artifacts.append({'name':r['target']['name'],'kind':r['target']['kind'],'path':r['executable']})
 (o/'artifacts.json').write_text(json.dumps(artifacts,indent=2)+'\n')
 def binary(name):
  matches=[r['path'] for r in artifacts if r['name']==name];assert len(matches)==1,(name,matches);return matches[0]
 run([binary('wamn_receiving_tests'),'route_authentication_live::cluster::startup_case::tests::','route_authentication_live::overlay_compatibility::required_contract_observation_refuses_changed_consumed_fields_and_constraints','--nocapture'],'app-tests.log')
 run([binary('receiving_command_histories_live'),'shrinking_preserves_the_original_business_property_and_outcome','infrastructure_failure_cannot_replace_a_business_counterexample','--exact','--nocapture'],'history-tests.log')
 old=subprocess.check_output(['git','show','HEAD:tests/integration/tests/startup_burst_live.rs'],cwd=w,text=True)
 new=(w/'tests/integration/tests/startup_burst_live.rs').read_text()
 prefix='#[derive(Deserialize)]';suffix='\n}\n'
 def definition(s):
  a=s.index(prefix);b=s.index(suffix,a)+len(suffix);return s[a:b]
 olddef=definition(old);newdef=definition(new)
 fields=re.findall(r'pub\(crate\) (\w+): ([^,]+),',olddef)
 fixture={name:(3 if kind=='usize' else {'x':'y'} if kind=='Value' else 'boundary-value') for name,kind in fields}
 def program(s,id_name):
  literal='r#'+json.dumps(json.dumps(fixture))+ '#'
  # A raw string literal contains the exact private-free fixture JSON.
  literal='r###"'+json.dumps(fixture)+'"###'
  template='"wamn dev returned the wrong product receipt: {'+('receipts' if id_name=='proof_id' else 'results')+':?}; stdout={stdout:?}"'
  local='receipts' if id_name=='proof_id' else 'results'
  return '''#![allow(dead_code)]
use std::path::PathBuf;
use serde::Deserialize;
use serde_json::{json,Value};
'''+definition(s)+'''
fn inspect(value:Value)->Value {match serde_json::from_value::<Inputs>(value) {Ok(input)=>json!({"accepted":input.'''+id_name+'''}),Err(error)=>json!({"error":error.to_string()})}}
fn main(){let fixture:Value=serde_json::from_str('''+literal+''').unwrap();
let mut missing=fixture.clone();missing.as_object_mut().unwrap().remove("proof_id");
let mut renamed=missing.clone();renamed["test_id"]=json!("boundary-value");
let mut wrong=fixture.clone();wrong["proof_id"]=json!(3);
let mut unknown=fixture.clone();unknown["extra"]=json!(true);
let rows=vec![inspect(fixture),inspect(missing),inspect(renamed),inspect(wrong),inspect(unknown),inspect(json!([])),inspect(json!(true))];
let mut diagnostics=Vec::new();for stdout in ["", "run completed: wrong\\n", "run completed: one\\nrun completed: two\\n"] {let '''+local+'''=stdout.lines().filter(|line|line.starts_with("run completed:")).collect::<Vec<_>>();diagnostics.push(format!('''+template+'''));}
println!("{}",json!({"inputs":rows,"diagnostics":diagnostics}));}
'''
 for phase,s,id_name in [('before',old,'proof_id'),('after',new,'test_id')]:
  probe=Path('/tmp/consolidation-app-test-names-'+phase+'.rs');probe.write_text(program(s,id_name));exe=probe.with_suffix('')
  run(['rustc','+1.98.0','--edition=2024',str(probe),'-o',str(exe),'-L','dependency='+str(w/'target/debug/deps'),'--extern','serde='+libs['serde'],'--extern','serde_json='+libs['serde_json']],phase+'-boundary-build.log')
  run([str(exe)],phase+'-boundary.json')
 before=(o/'before-boundary.json').read_bytes();after=(o/'after-boundary.json').read_bytes();assert before==after
 (o/'boundary-comparison.json').write_text(json.dumps({'source_type':'tests/integration/tests/startup_burst_live.rs::Inputs','method':'Compile the exact before/after derived Inputs definitions and the exact changed dev diagnostic template.','input_cases':7,'diagnostic_cases':3,'bytes':len(after),'sha256':hashlib.sha256(after).hexdigest(),'equal':True,'services_started':False},indent=2)+'\n')
finally:
 (o/'source-after.json').write_text(json.dumps({'unchanged':all(hashlib.sha256((w/p).read_bytes()).hexdigest()==r['sha256'] for p,r in source.items())},indent=2)+'\n')
