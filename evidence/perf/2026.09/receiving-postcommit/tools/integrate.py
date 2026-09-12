from pathlib import Path
import hashlib, json, os, stat, subprocess, sys
root=Path('/home/kaalin/dev/wamn')
base,head,prefix=sys.argv[1:]
assert prefix.startswith('docs/perf/2026.09/') and prefix.endswith('/')
evidence=root/(prefix+'main-landing-001')
active=('docs/perf/2026.09/ctc8-16-http-reuse/', 'docs/perf/2026.09/ctc8-33-nested-authority/', 'docs/perf/2026.09/native-b-adoption/', 'docs/perf/2026.09/native-f-retained/', prefix)
def git(*args): return subprocess.check_output(['git',*args],cwd=root)
def paths(*args): return set(filter(None,git(*args,'-z').decode().split('\0')))
def snapshot(names):
    result={}
    for n in sorted(names):
        p=root/n
        if p.is_symlink(): result[n]={'symlink':os.readlink(p)}
        elif p.exists(): result[n]={'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
        else: result[n]=None
    return result
assert git('rev-parse','HEAD').decode().strip()==base
assert git('branch','--show-current').decode().strip()=='main'
subprocess.run(['git','merge-base','--is-ancestor',base,head],cwd=root,check=True)
changed=paths('diff','--no-renames','--name-only',base,head)
dirty=paths('diff','--no-renames','--name-only','HEAD')
assert not changed&dirty
untracked=paths('ls-files','--others','--exclude-standard')
collisions=changed&untracked
assert all(n.startswith(prefix) for n in collisions)
backups={}
for n in collisions:
    p=root/n
    assert p.is_file() and not p.is_symlink()
    data=p.read_bytes()
    assert data==git('show',f'{head}:{n}'),n
    backups[n]=(data,stat.S_IMODE(p.stat().st_mode))
stable={n for n in dirty|untracked if n not in changed and not n.startswith(active)}
before=snapshot(stable)
index_before={line for line in git('ls-files','--stage','-z').split(b'\0') if line and line.split(b'\t',1)[1].decode() not in changed}
fenced=[n for n in changed if n.startswith(('services/host/','services/executor/','crates/platform/runtime/','deploy/')) or n=='crates/execution/host/src/router_driver.rs']
assert not fenced,fenced
evidence.mkdir(parents=True,exist_ok=False)
(evidence/'before.json').write_text(json.dumps({'base':base,'head':head,'stable_files':before,'reconciled_paths':sorted(collisions),'concurrent_evidence_not_compared':active},indent=2)+'\n')
try:
    for n in collisions: (root/n).unlink()
    result=subprocess.run(['git','merge','--ff-only','--no-stat',head],cwd=root,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    (evidence/'merge.log').write_bytes(result.stdout)
    result.check_returncode()
except BaseException:
    for n,(data,mode) in backups.items():
        if not (root/n).exists():
            (root/n).write_bytes(data)
            (root/n).chmod(mode)
    raise
for n,(_,mode) in backups.items(): (root/n).chmod(mode)
assert snapshot(stable)==before
index_after={line for line in git('ls-files','--stage','-z').split(b'\0') if line and line.split(b'\t',1)[1].decode() not in changed}
assert index_before==index_after
assert git('rev-parse','HEAD').decode().strip()==head
assert not paths('diff','--no-renames','--name-only','HEAD')&changed
result={'passed':True,'base':base,'head':head,'preserved_stable_files':len(stable),'preserved_index_entries':len(index_before),'reconciled_own_files':len(collisions),'concurrent_evidence_not_compared':active,'fenced_files':fenced}
(evidence/'result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))
