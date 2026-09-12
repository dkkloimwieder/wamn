import json,os,pathlib,subprocess,time
repo=pathlib.Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
out=pathlib.Path(__file__).parent
head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip()
status=subprocess.check_output(['git','status','--porcelain'],cwd=repo,text=True)
assert head=='c6808b12f3a2c59093541593f12ed227c82afa30' and not status
argv=['cargo','clippy','-p','wamn-host','--all-targets','--locked','--offline']
receipt={'source':head,'argv':argv,'started_unix_ns':time.time_ns(),'exit_code':None}
(out/'summary.json').write_text(json.dumps(receipt,indent=2)+'\n')
with (out/'clippy.log').open('wb') as log:
 result=subprocess.run(argv,cwd=repo,env=dict(os.environ,CARGO_BUILD_JOBS='4',RUSTC_WRAPPER=''),stdout=log,stderr=subprocess.STDOUT)
receipt.update(exit_code=result.returncode,finished_unix_ns=time.time_ns(),source_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip(),status_after=subprocess.check_output(['git','status','--porcelain'],cwd=repo,text=True))
(out/'summary.json').write_text(json.dumps(receipt,indent=2)+'\n')
raise SystemExit(result.returncode)
