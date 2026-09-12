import datetime, hashlib, json, os, pathlib, subprocess, time
out=pathlib.Path(__file__).resolve().parent
root=out.parents[4]
paths=subprocess.check_output(['git','diff','--name-only'],cwd=root,text=True).splitlines()
paths += ['deploy/sql/catalog-schema-prefix.sql','deploy/sql/control-portable-store-prefix.sql','deploy/sql/reject-immutable-row-change.sql']
source={'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':{n:{'sha256':hashlib.sha256((root/n).read_bytes()).hexdigest(),'mode':oct((root/n).stat().st_mode&0o777)} for n in sorted(set(paths))}}
(out/'source.json').write_text(json.dumps(source,indent=2)+'\n')
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))}
env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
commands=[
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-schema-control','--lib','run_plane::tests','--','--nocapture'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-proof-conformance','--test','state_ownership','--','--nocapture'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-ctl','--lib','--test','publish_release_live','--no-run','--message-format=json-render-diagnostics'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-control-provision','--test','control_portable_store','--no-run','--message-format=json-render-diagnostics'],
]
records=[]
for number,argv in enumerate(commands,1):
    start=time.monotonic(); stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
    log=out/f'command-{number}.log'
    with log.open('wb') as stream:
        os.chmod(log,0o600)
        result=subprocess.run(argv,cwd=root,env=env,stdout=stream,stderr=subprocess.STDOUT)
    record={'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':result.returncode,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}}
    records.append(record)
    (out/'commands.json').write_text(json.dumps(records,indent=2)+'\n')
    print(json.dumps({'command':number,**record}),flush=True)
    if result.returncode:break
unchanged=all(hashlib.sha256((root/n).read_bytes()).hexdigest()==v['sha256'] for n,v in source['files'].items())
(out/'source-after.json').write_text(json.dumps({'unchanged':unchanged},indent=2)+'\n')
raise SystemExit(any(r['exit'] for r in records))
