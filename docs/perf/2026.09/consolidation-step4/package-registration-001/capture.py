import datetime, hashlib, json, os, pathlib, subprocess, time
root=pathlib.Path(__file__).resolve().parents[5]
out=pathlib.Path(__file__).resolve().parent
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))}
env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
commands=[['cargo','+1.98.0','test','--locked','--offline','-p','wamn-schema-control','--lib','package_migrations::tests','--','--nocapture'],['cargo','+1.98.0','test','--locked','--offline','-p','wamn-ctl','--lib','--test','apply_package_live','--test','publish_release_live','--no-run','--message-format=json-render-diagnostics']]
source={}
paths=subprocess.check_output(['git','diff','--name-only'],cwd=root,text=True).splitlines()+['services/ctl/src/apply_package/registration_tests.rs']
for name in paths:
 p=root/name
 if p.is_file(): source[name]={'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'mode':oct(p.stat().st_mode&0o777)}
(out/'source.json').write_text(json.dumps({'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':source},indent=2)+'\n')
records=[]
for i,argv in enumerate(commands,1):
 start=time.monotonic();started=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with (out/f'command-{i}.log').open('wb') as log: result=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
 records.append({'argv':argv,'cwd':str(root),'started':started,'elapsed_seconds':time.monotonic()-start,'exit':result.returncode,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}})
 (out/'build-results.json').write_text(json.dumps(records,indent=2)+'\n')
 if result.returncode: raise SystemExit(result.returncode)
