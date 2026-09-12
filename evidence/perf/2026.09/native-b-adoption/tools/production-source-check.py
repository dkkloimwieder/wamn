from pathlib import Path
import subprocess
root=Path('/home/kaalin/.cache/wamn-lanes/native-b-adoption-20260910')
paths=set(subprocess.check_output(['git','diff','--name-only','79879412','--','*.rs'],cwd=root,text=True).splitlines())
paths.update(subprocess.check_output(['git','ls-files','--others','--exclude-standard','--','*.rs'],cwd=root,text=True).splitlines())
commands=[['rustfmt','--edition','2024','--config','skip_children=true','--check',*sorted(paths)], ['bash','-n','tools/journey-trace.sh','tools/journey-trace-proof'], ['git','diff','--check','--','.',':(exclude)docs/perf']]
for command in commands:
 print('command:',repr(command),flush=True)
 result=subprocess.run(command,cwd=root)
 print('exit:',result.returncode,flush=True)
 if result.returncode:
  raise SystemExit(result.returncode)
print('source-check result=pass rust_paths='+str(len(paths)))
