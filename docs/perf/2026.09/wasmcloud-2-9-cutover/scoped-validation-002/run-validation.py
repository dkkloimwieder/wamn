import hashlib,json,os,pathlib,subprocess,time
repo=pathlib.Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909');out=pathlib.Path(__file__).parent
paths=['crates/platform/runtime/src/engine.rs','tests/integration/src/virtualized_std_guest.rs','crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs','tools/wms-cluster-journey-run']
source={'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip(),'dirty_paths':subprocess.check_output(['git','status','--porcelain'],cwd=repo,text=True).splitlines(),'sha256':{p:hashlib.sha256((repo/p).read_bytes()).hexdigest() for p in paths}}
commands=[('deadline-memory',['cargo','test','-p','wamn-runtime','--lib','--locked','--offline','engine::tests::dropping_a_store_after_epoch_interruption_releases_its_memory','--','--exact','--include-ignored','--nocapture','--test-threads=1']),('clippy',['cargo','clippy','-p','wamn-runtime','-p','wamn-proof-integration','--all-targets','--locked','--offline'])]
env=dict(os.environ,CARGO_BUILD_JOBS='4',RUSTC_WRAPPER='')
summary={'source':source,'started_unix_ns':time.time_ns(),'commands':[]}
for name,argv in commands:
 row={'name':name,'argv':argv,'started_unix_ns':time.time_ns()};summary['commands'].append(row)
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
 with (out/(name+'.log')).open('wb') as log:
  try:r=subprocess.run(argv,cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=600);row['exit_code']=r.returncode
  except subprocess.TimeoutExpired:row['exit_code']=124;row['timeout']=True
 row['finished_unix_ns']=time.time_ns();print(name,row['exit_code'],flush=True)
 if row['exit_code'] != 0:break
summary['finished_unix_ns']=time.time_ns();summary['source_unchanged']=all(hashlib.sha256((repo/p).read_bytes()).hexdigest()==h for p,h in source['sha256'].items())
summary['verdict']='pass' if len(summary['commands'])==len(commands) and all(r['exit_code']==0 for r in summary['commands']) and summary['source_unchanged'] else 'fail'
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
