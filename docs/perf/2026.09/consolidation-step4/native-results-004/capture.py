import datetime,hashlib,json,os,pathlib,subprocess,time
out=pathlib.Path(__file__).resolve().parent;root=out.parents[4]
paths=json.loads(pathlib.Path('/tmp/consolidation-native-results-files.json').read_text())
source={'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':{p:{'sha256':hashlib.sha256((root/p).read_bytes()).hexdigest(),'mode':oct((root/p).stat().st_mode&0o777)} for p in paths}}
(out/'source.json').write_text(json.dumps(source,indent=2)+'\n')
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
commands=[
 ['cargo','+1.98.0','run','--locked','--offline','-p','wamn-authoring-model','--example','print-authoring-surface-schema'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-authoring-model','--test','contract','--','--nocapture'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-ctl','--lib','dev','--','--nocapture'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-ctl','--lib','push_component::tests::admission_debug_exposes_identity_without_saved_bytes_or_paths','--','--exact','--nocapture'],
 ['cargo','+1.98.0','test','--locked','--offline','-p','wamn-scenario-worker','--lib','management::tests','--','--nocapture'],
]
records=[]
for number,argv in enumerate(commands,1):
    start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
    with (out/f'command-{number}.log').open('wb') as log:
        os.chmod(log.name,0o600)
        if number==1:
            with (out/'authoring-after.json').open('wb') as schema: result=subprocess.run(argv,cwd=root,env=env,stdout=schema,stderr=log)
        else:result=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
    record={'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':result.returncode,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}}
    records.append(record);(out/'commands.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps({'command':number,**record}),flush=True)
    if result.returncode:break
    if number==1:
        before=(out/'authoring-before.json').read_bytes();after=(out/'authoring-after.json').read_bytes();assert before==after
        checked=json.loads((out/'schema-source.json').read_text())['files']
        assert all(hashlib.sha256((root/p).read_bytes()).hexdigest()==r['sha256'] for p,r in checked.items())
        (out/'schema-comparison.json').write_text(json.dumps({'generated_bytes':len(after),'generated_sha256':hashlib.sha256(after).hexdigest(),'generated_bytes_unchanged':True,'checked_in_files_unchanged':checked},indent=2)+'\n')
unchanged=all(hashlib.sha256((root/p).read_bytes()).hexdigest()==r['sha256'] for p,r in source['files'].items())
(out/'source-after.json').write_text(json.dumps({'unchanged':unchanged},indent=2)+'\n')
raise SystemExit(any(r['exit'] for r in records))
