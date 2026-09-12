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
assert head=='58163a39c5e91671112f4e36bf9c298483f787cb',head
before=snapshot();rows=[]
(evidence/'source-before.json.gz').write_bytes(gzip.compress((json.dumps(before,sort_keys=True)+'\n').encode(),mtime=0))
(evidence/'status-before.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
write('environment.json',{'removed_names':sorted(set(os.environ)-set(env)),'controlled':{k:env[k] for k in ('RUSTUP_TOOLCHAIN','RUSTC_WRAPPER','CARGO_BUILD_JOBS')},'target':str(root/'target'),'profile':'debug','armed_names':[]})
common=['cargo','+1.98.0','test','--locked','--offline']
cases=[
 ('membership',common+['-p','wamn-integration-tests','--lib','membership_test::','--','--nocapture']),
 ('host-session',common+['-p','wamn-integration-tests','--lib','host_session_test::','--','--nocapture']),
 ('identity-session',common+['-p','wamn-integration-tests','--lib','identity_session_test::','--','--nocapture']),
 ('trace',common+['-p','wamn-system-tests','--lib','trace_test::','--','--nocapture']),
 ('orchestrator-build',['cargo','+1.98.0','build','--locked','--offline','-p','wamn-gates','--bin','wamn-gates']),
]
binary=root/'target/debug/wamn-gates'
for command in ('','membershipproof','dashproof','host-session-proof','identity-jwks','identity-session','traceproof','serve-echo','identity-session-fixture'):
 cases.append(('help-'+(command or 'root'),[str(binary)]+([command] if command else [])+['--help']))

try:
 for i,(name,argv) in enumerate(cases):
  code=run(name,argv,env)
  err=(evidence/(name+'.stderr')).read_text(errors='replace')
  if code:
   write('stopped.json',{'name':name,'reason':'command failed','later_commands_unexecuted':[n for n,_ in cases[i+1:]]})
   break
finally:
 after=snapshot();head_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
 (evidence/'source-after.json.gz').write_bytes(gzip.compress((json.dumps(after,sort_keys=True)+'\n').encode(),mtime=0))
 (evidence/'status-after.txt').write_bytes(subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root))
 write('source-stability.json',{'head_before':head,'head_after':head_after,'head_unchanged':head==head_after,'all_tracked_bytes_modes_unchanged':before==after,'tracked_count':len(before),'changed_paths':[n for n in set(before)|set(after) if before.get(n)!=after.get(n)]})
 if binary.is_file():
  data=binary.read_bytes();write('binary.json',{'path':str(binary),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'mode':oct(stat.S_IMODE(binary.stat().st_mode)),'build_result_present':(evidence/'orchestrator-build-result.json').is_file()})
 print('capture complete',flush=True)
