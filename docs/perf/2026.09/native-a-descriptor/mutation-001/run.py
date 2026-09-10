from pathlib import Path
import hashlib, json, os, subprocess, time
lane=Path('/home/kaalin/.cache/wamn-lanes/native-a-descriptor-20260910')
mutation=Path(__file__).resolve().parent
restore=mutation.parent/'restore-001'
paths=['services/host/src/host.rs','services/executor/src/lib.rs']
originals={p:(lane/p).read_bytes() for p in paths}
expected=json.loads((mutation.parent/'build-001/inputs.json').read_text())['source_files']
assert all(hashlib.sha256(data).hexdigest()==expected[p] for p,data in originals.items())
backup=lane/'target/native-a-source-backup'
backup.mkdir(exist_ok=False)
for i,(p,data) in enumerate(originals.items()):
    (backup/str(i)).write_bytes(data)
command=['cargo','build','-p','wamn-host','-p','wamn-executor','--locked','--offline']
env=os.environ.copy()
env['CARGO_TARGET_DIR']=str(lane/'target')
result={'source_base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=lane,text=True).strip(),'original_sha256':expected,'replacement':'raise_descriptor_limit() -> Some(256_usize)','command':command}
def build(directory):
    started=time.time()
    with (directory/'build.log').open('wb') as log:
        completed=subprocess.run(command,cwd=lane,env=env,stdout=log,stderr=subprocess.STDOUT)
    (directory/'build-result.json').write_text(json.dumps({'exit_code':completed.returncode,'elapsed_seconds':time.time()-started},indent=2)+'\n')
    return completed.returncode
def prove(directory):
    cmd=['tools/native-descriptor-limit-check','--host-binary','target/debug/wamn-host','--executor-binary','target/debug/wamn-run-worker','--output-dir',str(directory/'proof')]
    with (directory/'proof.log').open('wb') as log:
        completed=subprocess.run(cmd,cwd=lane,stdout=log,stderr=subprocess.STDOUT)
    return completed.returncode, json.loads((directory/'proof/result.json').read_text())
try:
    old=b'let descriptor_soft_limit = wash_runtime::host::quota::raise_descriptor_limit();'
    new=b'let descriptor_soft_limit = Some(256_usize);'
    for p,data in originals.items():
        assert data.count(old)==1
        (lane/p).write_bytes(data.replace(old,new,1))
    (mutation/'source.patch').write_bytes(subprocess.check_output(['git','diff','HEAD','--',*paths],cwd=lane))
    result['mutant_build_exit']=build(mutation)
    if result['mutant_build_exit']==0:
        code,proof=prove(mutation)
        failed={c['name'] for c in proof['cases'] if not c['passed']}
        result['mutant_proof_exit']=code
        result['failed_cases']=sorted(failed)
        result['distinguished']=code==1 and failed=={'host_raises_low_soft','executor_raises_low_soft'} and proof['parent_nofile_unchanged']
finally:
    for p,data in originals.items():
        (lane/p).write_bytes(data)
        assert hashlib.sha256((lane/p).read_bytes()).hexdigest()==expected[p]
    result['source_restored']=True
    (mutation/'result.json').write_text(json.dumps(result,indent=2)+'\n')
restored={'build_exit':build(restore),'source_sha256':expected}
if restored['build_exit']==0:
    code,proof=prove(restore)
    restored['proof_exit']=code
    restored['passed']=code==0 and proof['passed']
(restore/'result.json').write_text(json.dumps(restored,indent=2)+'\n')
raise SystemExit(0 if result.get('distinguished') and restored.get('passed') else 1)
