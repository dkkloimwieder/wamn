import hashlib,json,os,pathlib,subprocess,sys,time
root=pathlib.Path(sys.argv[1]).resolve();out=pathlib.Path(sys.argv[2]).resolve();expected=sys.argv[3];argv=sys.argv[4:]
out.mkdir(parents=True,exist_ok=False)
def source():
 return {'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'tracked_source_status':subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=no','--','.',' :(exclude).beads'.strip()],cwd=root,text=True)}
before=source();assert before=={'head':expected,'tracked_source_status':''},before
(out/'source-before.json').write_text(json.dumps(before,indent=2)+'\n')
env=os.environ.copy();clear=['CARGO_TARGET_DIR','CARGO_BUILD_TARGET','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTDOCFLAGS','CARGO_ENCODED_RUSTDOCFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTC_BOOTSTRAP']
for key in clear:env.pop(key,None)
env['CARGO_BUILD_JOBS']='4'
(out/'command.json').write_text(json.dumps({'cwd':str(root),'argv':argv,'cleared_environment_keys':clear,'environment':{'CARGO_BUILD_JOBS':'4'},'source_excludes':['.beads'],'capture_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest()},indent=2)+'\n')
start=time.monotonic();print(json.dumps({'started':argv,'evidence':str(out)}),flush=True)
with (out/'stdout.log').open('w') as stdout,(out/'stderr.log').open('w') as stderr:
 p=subprocess.run(argv,cwd=root,env=env,stdout=stdout,stderr=stderr)
after=source();(out/'source-after.json').write_text(json.dumps(after,indent=2)+'\n')
result={'exit_code':p.returncode,'elapsed_seconds':round(time.monotonic()-start,3),'source_unchanged':before==after}
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result),flush=True)
raise SystemExit(p.returncode if before==after else 125)
