import ast, datetime, gzip, hashlib, json, os, pathlib, stat, subprocess, time
root=pathlib.Path('/home/kaalin/.cache/wamn-lanes/consolidation-postgres-claims-20260912')
evidence=pathlib.Path(__file__).resolve().parent
env={k:v for k,v in os.environ.items() if not k.startswith(('WAMN_','OTEL_','PG','GIT_')) and k not in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR')}
env.update(RUSTUP_TOOLCHAIN='1.98.0',RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
previous=pathlib.Path('/home/kaalin/dev/wamn/docs/perf/2026.09/consolidation-step4/postgres-claims-003/capture.py')
module=ast.parse(previous.read_text())
functions=[n for n in module.body if isinstance(n,ast.FunctionDef) and n.name in ('write','snapshot','run')]
exec(compile(ast.Module(body=functions,type_ignores=[]),str(previous),'exec'))
head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
assert head=='cc82d0ed5d0b6ff9a74e9492e67d065e9c5eb1e0',head
before=snapshot();rows=[]
(evidence/'source-before.json.gz').write_bytes(gzip.compress((json.dumps(before,sort_keys=True)+'\n').encode(),mtime=0))
(evidence/'status-before.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
write('environment.json',{'removed_names':sorted(set(os.environ)-set(env)),'controlled':{k:env[k] for k in ('RUSTUP_TOOLCHAIN','RUSTC_WRAPPER','CARGO_BUILD_JOBS')},'target':str(root/'target'),'profile':'debug','armed_names':[]})
common=['cargo','+1.98.0','test','--locked','--offline']
cases=[('runtime-policy',common+['-p','wamn-conformance-tests','--lib','runtime_policy::','--','--nocapture']),('deployment-declarations',common+['-p','wamn-system-tests','--test','deployment_declarations','--','--nocapture'])]
try:
 for i,(name,argv) in enumerate(cases):
  code=run(name,argv,env)
  err=(evidence/(name+'.stderr')).read_text(errors='replace')
  if code and ('could not compile' in err or 'failed to run custom build command' in err):
   write('stopped.json',{'name':name,'reason':'compile failure','later_commands_unexecuted':[n for n,_ in cases[i+1:]]})
   break
finally:
 after=snapshot();head_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
 (evidence/'source-after.json.gz').write_bytes(gzip.compress((json.dumps(after,sort_keys=True)+'\n').encode(),mtime=0))
 (evidence/'status-after.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
 write('source-stability.json',{'head_before':head,'head_after':head_after,'head_unchanged':head==head_after,'all_tracked_bytes_modes_unchanged':before==after,'tracked_count':len(before),'changed_paths':[n for n in set(before)|set(after) if before.get(n)!=after.get(n)]})
 print('capture complete',flush=True)
