from pathlib import Path
import json, subprocess, sys
p=Path(sys.argv[1]); tree=sys.argv[2]
for name,command in json.loads((p/'validation-commands.json').read_text()):
    argv=['python3',str(p/'tools/capture.py'),'--tree',tree,'--evidence-dir',str(p/name),'--',*command]
    result=subprocess.run(argv)
    if result.returncode:
        (p/'validation-sequence-result.json').write_text(json.dumps({'passed':False,'stopped_at':name,'exit_code':result.returncode})+'\n')
        raise SystemExit(result.returncode)
(p/'validation-sequence-result.json').write_text(json.dumps({'passed':True})+'\n')
