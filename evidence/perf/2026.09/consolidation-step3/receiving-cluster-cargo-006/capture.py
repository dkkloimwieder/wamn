import datetime,gzip,hashlib,json,os,pathlib,re,signal,stat,subprocess,time
signal.signal(signal.SIGHUP,signal.SIG_IGN)
os.umask(0o077)
root=pathlib.Path('/home/kaalin/dev/wamn')
cwd=pathlib.Path('/home/kaalin/.cache/wamn-lanes/receiving-postcommit-final-20260910')
out=root/'docs/perf/2026.09/consolidation-step3/receiving-cluster-cargo-006'
live=root/'docs/perf/2026.09/consolidation-step3/receiving-cluster-live-006'
expected='fb6c2b2e28fbcb90a8a373672a60e2397488cae5'
def now(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
def write(path,value): path.write_text(json.dumps(value,indent=2)+'\n')
def git(*args): return subprocess.check_output(['git',*args],cwd=cwd).decode().strip()
def snapshot():
    files={}
    paths=subprocess.check_output(['git','ls-files','-z'],cwd=cwd).split(b'\0')
    for raw in paths:
        if not raw or raw.startswith(b'.beads/'): continue
        name=os.fsdecode(raw);path=cwd/name;st=path.lstat()
        data=os.fsencode(os.readlink(path)) if stat.S_ISLNK(st.st_mode) else path.read_bytes()
        files[name]={'sha256':hashlib.sha256(data).hexdigest(),'mode':stat.S_IMODE(st.st_mode),'type':stat.S_IFMT(st.st_mode)}
    return files
assert git('rev-parse','HEAD')==expected
assert not git('status','--porcelain=v1','--untracked-files=all')
assert not live.exists()
out.mkdir()
write(out/'source.json',{'source_commit':expected,'source_directory':str(cwd),'controller_pid':os.getpid(),'captured_utc':now()})
before=snapshot()
with gzip.open(out/'source-before.json.gz','wt') as f: json.dump(before,f,sort_keys=True)
env={k:v for k,v in os.environ.items() if not k.startswith(('WAMN_','OTEL_','PG')) and k not in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR')}
env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
common=['cargo','+1.98.0','test','--locked','--offline','-p','wamn-receiving-tests','--lib']
def run(argv,directory,settings):
    directory.mkdir(exist_ok=True)
    record=dict(source_commit=expected,source_directory=str(cwd),command=argv,environment=settings,started_utc=now(),controller_pid=os.getpid())
    write(directory/'command.json',record)
    started=time.monotonic()
    with (directory/'cargo.log').open('xb') as log:
        completed=subprocess.run(argv,cwd=cwd,env={**env,**settings},stdout=log,stderr=subprocess.STDOUT,check=False)
    record.update(exit_code=completed.returncode,elapsed_seconds=time.monotonic()-started,finished_utc=now())
    write(directory/'record.json',record)
    print(json.dumps(record),flush=True)
    return completed.returncode
ordinary=run(common+['route_authentication_live::cluster::startup_case::tests','--','--nocapture'],out/'ordinary',{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'})
if ordinary==0:
    code=run(common+['route_authentication_live::cluster::default_case::released_routes_materializer_startup_and_environment_isolation','--','--exact','--ignored','--nocapture'],out,{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2','WAMN_RECEIVING_EVIDENCE_DIR':str(live)})
else:
    code=ordinary
    write(out/'record.json',{'source_commit':expected,'exit_code':code,'full_default_executed':False,'reason':'ordinary startup tests failed','finished_utc':now()})
after=snapshot()
with gzip.open(out/'source-after.json.gz','wt') as f: json.dump(after,f,sort_keys=True)
changed=[name for name in sorted(before.keys()|after.keys()) if before.get(name)!=after.get(name)]
head=git('rev-parse','HEAD');status=git('status','--porcelain=v1','--untracked-files=all')
write(out/'source-stability.json',{'source_before':expected,'source_after':head,'head_unchanged':head==expected,'tracked_files':len(before),'changed_paths':changed,'bytes_and_modes_unchanged':not changed,'status_after':status})
commands=[]
for argv in [['kind','get','clusters'],['docker','ps','-a','--format','{{.Names}}'],['docker','image','ls','--format','{{.Repository}}:{{.Tag}}']]:
    completed=subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False)
    commands.append({'command':argv,'exit_code':completed.returncode,'stdout':completed.stdout.decode(errors='replace'),'stderr':completed.stderr.decode(errors='replace')})
observation={'observed_utc':now(),'resource_commands':commands,'source_commit':head,'source_clean':not status,'mutation_performed':False}
source_path=live/'source.json'
if source_path.exists():
    source=json.loads(source_path.read_text());name=source['cluster']
    assert re.fullmatch(r'wamn-receiving-[0-9a-f]{32}',name)
    names=commands[1]['stdout'].splitlines();images=commands[2]['stdout'].splitlines()
    remaining=[x for x in names if x==name or x.startswith(name+'-')]
    owned_images=[x for x in images if x.endswith(':'+name)]
    observation.update(cluster=name,cluster_present=name in commands[0]['stdout'].splitlines(),owned_container_names=remaining,owned_image_names=owned_images,private_directory_exists=pathlib.Path('/tmp',name).exists(),frozen_wamn_present='wamn' in commands[0]['stdout'].splitlines())
    observation['result']='pass' if all(x['exit_code']==0 for x in commands) and not observation['cluster_present'] and not remaining and not owned_images and not observation['private_directory_exists'] and not status and head==expected and not changed else 'fail'
else:
    observation.update(result='no_cluster_created',helper_source_record_present=False)
write(out/'post-run-observation.json',observation)
print(json.dumps({'test_exit_code':code,'source_unchanged':not changed and head==expected,'cleanup_result':observation['result'],'finished_utc':now()}),flush=True)
raise SystemExit(code)
