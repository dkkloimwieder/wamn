import ast, gzip, json, os, pathlib, subprocess
root=pathlib.Path('/home/kaalin/.cache/wamn-lanes/consolidation-postgres-claims-20260912')
evidence=pathlib.Path(__file__).resolve().parent
env={k:v for k,v in os.environ.items() if not k.startswith(('WAMN_','OTEL_','PG','GIT_')) and k not in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR')}
env.update(RUSTUP_TOOLCHAIN='1.98.0',RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
previous=evidence.parent/'postgres-claims-003'/'capture.py'
module=ast.parse(previous.read_text())
functions=[n for n in module.body if isinstance(n,ast.FunctionDef) and n.name in ('write','snapshot','run')]
import datetime, hashlib, stat, time
for function in functions:
 for node in ast.walk(function):
  if isinstance(node,ast.Constant) and node.value == 'results.json': node.value='session-feature-results.json'
exec(compile(ast.Module(body=functions,type_ignores=[]),str(previous),'exec'))
head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
before=snapshot();rows=[]
(evidence/'session-feature-before.json.gz').write_bytes(gzip.compress((json.dumps(before,sort_keys=True)+'\n').encode(),mtime=0))
write('session-feature-environment.json',{'removed_names':sorted(set(os.environ)-set(env)),'controlled':{k:env[k] for k in ('RUSTUP_TOOLCHAIN','RUSTC_WRAPPER','CARGO_BUILD_JOBS')},'target':str(root/'target'),'profile':'debug','armed_names':[]})
try:
 run('session-route-feature',['cargo','+1.98.0','test','--locked','--offline','-p','wamn-runtime','--features','test-util,wasm_component_model_implements,wash-runtime/washlet,wash-runtime/wasi-config,wash-runtime/wasi-otel,wash-runtime/wasmcloud-nats','--test','session_route_authentication','--','--nocapture'],env)
finally:
 after=snapshot();head_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
 (evidence/'session-feature-after.json.gz').write_bytes(gzip.compress((json.dumps(after,sort_keys=True)+'\n').encode(),mtime=0))
 write('session-feature-stability.json',{'head_before':head,'head_after':head_after,'head_unchanged':head==head_after,'all_tracked_bytes_modes_unchanged':before==after,'tracked_count':len(before),'changed_paths':[n for n in set(before)|set(after) if before.get(n)!=after.get(n)]})
 print('session feature capture complete',flush=True)
