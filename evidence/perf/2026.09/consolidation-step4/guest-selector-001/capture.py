from pathlib import Path
import hashlib,json,os,stat,subprocess,time
root=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-wiring-20260912')
out=Path('/home/kaalin/dev/wamn/docs/perf/2026.09/consolidation-step4/guest-selector-001')
out.mkdir(parents=True,exist_ok=False)
paths=subprocess.check_output(['git','diff','--name-only'],cwd=root,text=True).splitlines()
def source():
 return {'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':{f:{'sha256':hashlib.sha256((root/f).read_bytes()).hexdigest(),'mode':oct(stat.S_IMODE((root/f).stat().st_mode))} for f in paths},'status':subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=root,text=True)}
before=source();(out/'source-before.json').write_text(json.dumps(before,indent=2)+'\n')
(out/'source.diff').write_bytes(subprocess.check_output(['git','diff'],cwd=root))
(out/'capture.py').write_bytes(Path(__file__).read_bytes())
env={k:v for k,v in os.environ.items() if not k.startswith(('WAMN_','PG','OTEL_')) and k not in {'DB_URL','DATABASE_URL'}}
env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2',CARGO_TARGET_DIR=str(root/'target'),KUBECONFIG='/dev/null')
commands=[['bash','-n','tools/build-components'],['python3','-c',"from pathlib import Path; p=Path('docs/perf/2026.09/effects-response/tools/cross_profile.py'); compile(p.read_text(), str(p), 'exec')"],['cargo','+1.98.0','test','--locked','--offline','-p','wamn-proof-conformance','--test','profile_selectors','--test','guest_workspace_closure','--','--nocapture','--test-threads=1'],['tools/build-components','watch-roots','all'],['tools/build-components','watch-roots','app','apps/wamn_receiving'],['cargo','+1.98.0','check','--locked','--offline','-p','wamn-receiving-tests','-p','wamn-wms-tests','--all-targets']]
results=[]
for i,argv in enumerate(commands,1):
 start=time.monotonic()
 with (out/f'command-{i}.stdout').open('wb') as stdout,(out/f'command-{i}.stderr').open('wb') as stderr:
  run=subprocess.run(argv,cwd=root,env=env,stdout=stdout,stderr=stderr)
 row={'argv':argv,'exit_code':run.returncode,'elapsed_seconds':time.monotonic()-start}
 results.append(row);(out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
 print(f'Command {i}: exit {run.returncode}, {row["elapsed_seconds"]:.3f} seconds',flush=True)
 if run.returncode:break
after=source();(out/'source-after.json').write_text(json.dumps(after,indent=2)+'\n')
assert after==before,'source changed during capture'
print('Source bytes, modes, and HEAD stayed unchanged',flush=True)
raise SystemExit(next((r['exit_code'] for r in results if r['exit_code']),0))
