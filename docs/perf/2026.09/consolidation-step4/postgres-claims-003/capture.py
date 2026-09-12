import datetime, gzip, hashlib, json, os, pathlib, socket, stat, subprocess, time
root=pathlib.Path('/home/kaalin/.cache/wamn-lanes/consolidation-postgres-claims-20260912')
evidence=pathlib.Path(__file__).resolve().parent
private=root.parent/'consolidation-postgres-claims-pg-003-20260912'
private.mkdir(mode=0o700)
pg=pathlib.Path('/usr/lib/postgresql/18/bin')

def write(name,value):
    (evidence/name).write_text(json.dumps(value,indent=2,sort_keys=True)+'\n')
def snapshot():
    paths=subprocess.check_output(['git','ls-files','-z'],cwd=root).decode().split('\0')
    result={}
    for name in paths:
        if not name or name.startswith('.beads/'): continue
        p=root/name;info=p.lstat()
        data=os.fsencode(os.readlink(p)) if stat.S_ISLNK(info.st_mode) else p.read_bytes()
        result[name]={'mode':oct(stat.S_IMODE(info.st_mode)),'kind':'symlink' if p.is_symlink() else 'file','bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()}
    return result
head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
before=snapshot()
(evidence/'source-before.json.gz').write_bytes(gzip.compress((json.dumps(before,sort_keys=True)+'\n').encode(),mtime=0))
(evidence/'status-before.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
env={k:v for k,v in os.environ.items() if not k.startswith(('WAMN_','OTEL_','PG','GIT_')) and k not in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR')}
env.update(RUSTUP_TOOLCHAIN='1.98.0',RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
write('environment.json',{'removed_names':sorted(set(os.environ)-set(env)),'controlled':{k:env[k] for k in ('RUSTUP_TOOLCHAIN','RUSTC_WRAPPER','CARGO_BUILD_JOBS')},'armed_names':['WAMN_PG_TEST_URL','WAMN_POOL_LIFECYCLE_PG_URL'],'target':str(root/'target'),'profile':'debug','benchmark_inputs':'unset'})
rows=[]
def run(name,argv,environment=env):
    started=datetime.datetime.now(datetime.timezone.utc).isoformat();clock=time.monotonic()
    write(name+'-command.json',{'argv':argv,'cwd':str(root),'source':head})
    print(json.dumps({'starting':name,'argv':argv}),flush=True)
    with (evidence/(name+'.stdout')).open('wb') as out,(evidence/(name+'.stderr')).open('wb') as err:
        result=subprocess.run(argv,cwd=root,env=environment,stdout=out,stderr=err)
    row={'name':name,'argv':argv,'source':head,'started_utc':started,'ended_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'elapsed_seconds':time.monotonic()-clock,'exit_code':result.returncode}
    for stream in ('stdout','stderr'):
        p=evidence/(name+'.'+stream);data=p.read_bytes();row[stream]={'path':p.name,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'mode':oct(stat.S_IMODE(p.stat().st_mode))}
    rows.append(row);write(name+'-result.json',row);write('results.json',rows)
    print(json.dumps(row),flush=True)
    return result.returncode
started=False
try:
    if run('initdb',[str(pg/'initdb'),'-D',str(private/'data'),'--auth-local=trust','--auth-host=trust','--no-locale','--encoding=UTF8']): raise RuntimeError('initdb failed')
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));port=s.getsockname()[1]
    options=f'-h 127.0.0.1 -p {port} -k {private} -c max_connections=100'
    if run('postgres-start',[str(pg/'pg_ctl'),'-D',str(private/'data'),'-l',str(evidence/'postgres.log'),'-o',options,'-w','start']): raise RuntimeError('PostgreSQL start failed')
    started=True
    url=f'postgresql://{os.environ["USER"]}@127.0.0.1:{port}/postgres'
    test_env=dict(env,WAMN_PG_TEST_URL=url,WAMN_POOL_LIFECYCLE_PG_URL=url)
    write('postgres.json',{'data':str(private/'data'),'socket_directory':str(private),'address':'127.0.0.1','port':port,'database':'postgres','version':subprocess.check_output([str(pg/'postgres'),'--version'],text=True).strip(),'scope':'fresh owned PostgreSQL server'})
    common=['cargo','+1.98.0','test','--locked','--offline','-p','wamn-runtime','--features','wasm_component_model_implements,wash-runtime/washlet,wash-runtime/wasi-config,wash-runtime/wasi-otel,wash-runtime/wasmcloud-nats']
    cases=[('claims',common+['--lib','plugins::wamn_postgres::claims::tests::','--','--nocapture','--test-threads=1']),('lifecycle',common+['--lib','plugins::wamn_postgres::claims::tests::live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving','--','--ignored','--exact','--nocapture']),('postgres-wit',common+['--test','postgres_wit_coherence']),('state-ownership',['cargo','+1.98.0','test','--locked','--offline','-p','wamn-proof-conformance','--test','state_ownership','--','--nocapture'])]
    for name,argv in cases:
        code=run(name,argv,test_env)
        err=(evidence/(name+'.stderr')).read_text(errors='replace')
        if code and ('could not compile' in err or 'failed to run custom build command' in err):
            write('stopped.json',{'name':name,'reason':'compile failure','later_commands_unexecuted':[n for n,_ in cases[cases.index((name,argv))+1:]]})
            break
finally:
    if started: run('postgres-stop',[str(pg/'pg_ctl'),'-D',str(private/'data'),'-m','fast','-w','stop'])
    after=snapshot();head_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
    (evidence/'source-after.json.gz').write_bytes(gzip.compress((json.dumps(after,sort_keys=True)+'\n').encode(),mtime=0))
    (evidence/'status-after.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
    write('source-stability.json',{'head_before':head,'head_after':head_after,'head_unchanged':head==head_after,'all_tracked_bytes_modes_unchanged':before==after,'tracked_count':len(before),'changed_paths':[n for n in set(before)|set(after) if before.get(n)!=after.get(n)]})
    print('capture complete',flush=True)
