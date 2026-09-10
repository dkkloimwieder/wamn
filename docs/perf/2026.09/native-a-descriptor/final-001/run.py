from pathlib import Path
import hashlib, json, os, subprocess, time
lane=Path('/home/kaalin/.cache/wamn-lanes/native-a-descriptor-20260910')
evidence=Path(__file__).resolve().parent
assert not subprocess.check_output(['git','status','--porcelain'],cwd=lane,text=True).strip()
command=['cargo','build','-p','wamn-host','-p','wamn-executor','--locked','--offline']
env=os.environ.copy()
env['CARGO_TARGET_DIR']=str(lane/'target')
identity={'source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=lane,text=True).strip(),'base_commit':subprocess.check_output(['git','rev-parse','HEAD^'],cwd=lane,text=True).strip(),'build_command':command,'source_sha256':{p:hashlib.sha256((lane/p).read_bytes()).hexdigest() for p in ['Cargo.toml','Cargo.lock','services/host/src/host.rs','services/executor/src/lib.rs','tools/native-descriptor-limit-check']}}
(evidence/'inputs.json').write_text(json.dumps(identity,indent=2)+'\n')
started=time.time()
with (evidence/'build.log').open('wb') as log:
    build=subprocess.run(command,cwd=lane,env=env,stdout=log,stderr=subprocess.STDOUT)
result={'build_exit':build.returncode,'build_seconds':time.time()-started}
if build.returncode==0:
    proof_command=['tools/native-descriptor-limit-check','--host-binary','target/debug/wamn-host','--executor-binary','target/debug/wamn-run-worker','--output-dir',str(evidence/'proof')]
    result['proof_command']=proof_command
    with (evidence/'proof.log').open('wb') as log:
        proof=subprocess.run(proof_command,cwd=lane,stdout=log,stderr=subprocess.STDOUT)
    result['proof_exit']=proof.returncode
    result['proof_counts']=json.loads((evidence/'proof/result.json').read_text())['counts']
(evidence/'result.json').write_text(json.dumps(result,indent=2)+'\n')
raise SystemExit(0 if result.get('build_exit')==0 and result.get('proof_exit')==0 else 1)
