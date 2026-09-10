from pathlib import Path
import hashlib, json, os, subprocess, time
lane=Path('/home/kaalin/.cache/wamn-lanes/native-a-descriptor-20260910')
evidence=Path(__file__).resolve().parent
command=['cargo','build','-p','wamn-host','-p','wamn-executor','--locked','--offline']
env=os.environ.copy()
env['CARGO_TARGET_DIR']=str(lane/'target')
started=time.time()
identity={'command':command,'source_base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=lane,text=True).strip(),'source_files':{p:hashlib.sha256((lane/p).read_bytes()).hexdigest() for p in ['services/host/src/host.rs','services/executor/src/lib.rs']},'rustc':subprocess.check_output(['rustc','-Vv'],cwd=lane,text=True),'cargo':subprocess.check_output(['cargo','-V'],cwd=lane,text=True),'target_dir':env['CARGO_TARGET_DIR']}
(evidence/'inputs.json').write_text(json.dumps(identity,indent=2)+'\n')
result=subprocess.run(command,cwd=lane,env=env)
(evidence/'result.json').write_text(json.dumps({'exit_code':result.returncode,'elapsed_seconds':time.time()-started},indent=2)+'\n')
raise SystemExit(result.returncode)
