from pathlib import Path
import datetime,hashlib,json,os,subprocess,time
root=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912');out=root/'docs/perf/2026.09/consolidation-step4/relation-fields-001'
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
paths=json.loads(Path('/tmp/consolidation-relation-fields-files.json').read_text());source={p:{'sha256':hashlib.sha256((root/p).read_bytes()).hexdigest(),'mode':oct((root/p).stat().st_mode&0o777)} for p in paths}
(out/'source.json').write_text(json.dumps({'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':source},indent=2)+'\n')
records=[]
def run(argv,name):
 start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with (out/name).open('wb') as log:
  os.chmod(log.name,0o600);r=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
 record={'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':r.returncode,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}};records.append(record)
 (out/'commands.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps(record),flush=True)
 assert r.returncode==0
try:
 run(['cargo','+1.98.0','build','--locked','--offline','-p','wamn-schema-generator','--lib','--message-format=json-render-diagnostics'],'after-cargo.log')
 lib=None
 for line in (out/'after-cargo.log').read_text().splitlines():
  try:a=json.loads(line)
  except ValueError:continue
  if a.get('reason')=='compiler-artifact' and a['target']['name']=='wamn_schema_generator':lib=next(p for p in a['filenames'] if p.endswith('.rlib'))
 assert lib
 probe=out/'after-probe.rs';probe.write_text((out/'baseline-probe.rs').read_text().replace('DataAccessRelationInventory','DataAccessRelationFields').replace('derive_data_access_overlay_from_inventory','derive_data_access_overlay_from_relation_fields'));binary=Path('/tmp/consolidation-relation-fields-after')
 run(['rustc','+1.98.0','--edition=2024',str(probe),'-o',str(binary),'-L','dependency='+str(root/'target/debug/deps'),'--extern','wamn_schema_generator='+lib],'after-probe-build.log')
 comparisons={}
 for app in ['wamn_receiving','wamn_wms','client_acme_receiving']:
  manifest=root/'apps'/app/'wamn.json';overlay=manifest.parent/'generated/platform-policy/data-access.json';output=out/f'{app}-after.json'
  run([str(binary),str(manifest),str(overlay),str(output)],f'{app}-after.log')
  before=(out/f'{app}-before.json').read_bytes();after=output.read_bytes();assert before==after==overlay.read_bytes()
  comparisons[app]={'bytes':len(after),'sha256':hashlib.sha256(after).hexdigest(),'matches_base_and_checked_in':True}
 (out/'overlay-comparison.json').write_text(json.dumps(comparisons,indent=2)+'\n')
 run(['cargo','+1.98.0','test','--locked','--offline','-p','wamn-receiving-tests','-p','wamn-schema-generator','--test','generation','--','--nocapture'],'generation.log')
 run(['cargo','+1.98.0','test','--locked','--offline','-p','wamn-ctl','--lib','reconcile_package_data_access::tests','--','--nocapture'],'reconciliation.log')
 run(['cargo','+1.98.0','test','--locked','--offline','-p','wamn-receiving-tests','--lib','--no-run','--message-format=json-render-diagnostics'],'app-build.log')
 artifacts=[]
 for line in (out/'app-build.log').read_text().splitlines():
  try:a=json.loads(line)
  except ValueError:continue
  if a.get('reason')=='compiler-artifact' and a.get('executable') and a['profile']['test'] and a['target']['name']=='wamn_receiving_tests':artifacts.append(a['executable'])
 assert len(artifacts)==1
 binary=Path(artifacts[0]);(out/'app-artifact.json').write_text(json.dumps({'path':str(binary),'bytes':binary.stat().st_size,'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()},indent=2)+'\n')
 run(['pg_virtualenv','-t','-v','18','python3',str(out/'postgres-run.py'),str(binary)],'postgres-controller.log')
finally:
 unchanged=all(hashlib.sha256((root/p).read_bytes()).hexdigest()==r['sha256'] for p,r in source.items())
 (out/'source-after.json').write_text(json.dumps({'unchanged':unchanged},indent=2)+'\n')
 detail=out/'postgres-result.json'
 if detail.exists():
  pg=json.loads(detail.read_text());(out/'postgres-cleanup.json').write_text(json.dumps({'configuration_root':pg['postgres']['configuration_root'],'removed':not Path(pg['postgres']['configuration_root']).exists()},indent=2)+'\n')
