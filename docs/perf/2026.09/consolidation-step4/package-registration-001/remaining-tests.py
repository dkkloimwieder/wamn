import datetime,json,os,pathlib,subprocess,time
out=pathlib.Path(__file__).resolve().parent
root=out.parents[4]
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))}
env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
argv=['cargo','+1.98.0','test','--locked','--offline','-p','wamn-control-provision','--test','control_portable_store','--no-run','--message-format=json-render-diagnostics']
start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
with (out/'command-3.log').open('wb') as log:r=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
(out/'control-build-result.json').write_text(json.dumps({'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':r.returncode,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}},indent=2)+'\n')
if r.returncode:raise SystemExit(r.returncode)
raise SystemExit(subprocess.run(['python3',str(out/'postgres-run.py')],cwd=root,env=env).returncode)
