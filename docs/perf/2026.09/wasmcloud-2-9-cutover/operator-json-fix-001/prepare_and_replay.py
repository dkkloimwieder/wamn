from pathlib import Path
import ast, difflib, hashlib, json, runpy, subprocess, textwrap

root=Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
out=Path('/tmp/wamn-cutover-operator-json-prepared')
path='tools/receiving-operator-recovery-run'
old=(root/path).read_text()
before='''    expected = json.loads(run("distributed-crds-client-decode", kube + ["create", "--dry-run=client", "--validate=false", "-f",
                             str(source / "deployment-crds-001/helm-show-crds-2.9.0.stdout"), "-o", "json"]))
    expected = expected["items"]
'''
after='''    # kubectl create prints one JSON object per input CRD, not a List.
    expected = []
    decoder = json.JSONDecoder()
    remaining = run("distributed-crds-client-decode", kube + ["create", "--dry-run=client", "--validate=false", "-f",
                    str(source / "deployment-crds-001/helm-show-crds-2.9.0.stdout"), "-o", "json"]).decode().lstrip()
    while remaining:
        crd, end = decoder.raw_decode(remaining)
        expected.append(crd)
        remaining = remaining[end:].lstrip()
'''
assert old.count(before)==1
new=old.replace(before,after)
proposed=out/'receiving-operator-recovery-run.proposed'
proposed.write_text(new)
ast.parse(new)
patch=''.join(difflib.unified_diff(old.splitlines(keepends=True),new.splitlines(keepends=True),fromfile='a/'+path,tofile='b/'+path))
(out/'operator-json.patch').write_text(patch)
check=subprocess.run(['git','apply','--check',str(out/'operator-json.patch')],cwd=root,text=True,capture_output=True)
assert check.returncode==0,check.stderr
captured=Path('/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-004/journey/operator-recovery/0003-distributed-crds-client-decode.stdout')
raw=captured.read_bytes()
try:
    json.loads(raw)
except json.JSONDecodeError as error:
    prior_failure=str(error)
else:
    raise AssertionError('prior parser must reproduce the failure')
source=root/'docs/perf/2026.09/wasmcloud-2-9-cutover'
inventory=json.loads((source/'deployment-crds-001/crd-inventory.json').read_text())
module=runpy.run_path(str(proposed))
block=textwrap.dedent(new[new.index('    expected = []'):new.index('    installed = json.loads')])
compiled=compile(block,str(proposed), 'exec')

def replay(data):
    calls=[]
    def captured_run(label, arguments):
        assert label=='distributed-crds-client-decode'
        calls.append({'label':label,'arguments':arguments,'mode':'captured bytes; command not executed'})
        return data
    scope=dict(json=json,run=captured_run,kube=['kubectl'],source=source,inventory=inventory,require=module['require'])
    exec(compiled,scope)
    assert len(calls)==1
    return scope['expected'],calls

objects,calls=replay(raw)
rows=[]
for crd in objects:
    name=crd['metadata']['name']
    actual=module['digest'](crd['spec'])
    expected=inventory['versions']['2.9.0'][name]['spec_sha256']
    assert actual==expected
    rows.append({'kind':crd['kind'],'name':name,'actual_spec_sha256':actual,'expected_spec_sha256':expected})
assert replay(b' \n\t'+raw+b'\n\t ')[0]==objects
controls=[]
for label,data in [('empty',b''),('missing_document',b'\n'.join(json.dumps(v).encode() for v in objects[:-1])),
                   ('duplicate_document',raw+b'\n'+json.dumps(objects[0]).encode()),
                   ('truncated_document',raw.rstrip()[:-1]),('trailing_garbage',raw+b'\nnot-json')]:
    try:
        replay(data)
    except (json.JSONDecodeError,RuntimeError) as error:
        controls.append({'case':label,'refused':True,'error_type':type(error).__name__,'error':str(error)})
    else:
        raise AssertionError(label+' was accepted')
result={'scope':'Offline captured-output replay only; installed CRDs, outage, recovery and live acceptance were not exercised',
        'source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
        'captured_file':str(captured),'captured_bytes':len(raw),'captured_sha256':hashlib.sha256(raw).hexdigest(),
        'prior_json_loads_error':prior_failure,'replayed_exact_proposed_parser_and_inventory_guard':True,
        'decoded_crds':rows,'whitespace_variant_equal':True,'refusal_controls':controls,'captured_command_stub':calls,
        'source_parse':'pass','git_apply_check_exit_code':check.returncode,
        'patch_sha256':hashlib.sha256(patch.encode()).hexdigest(),
        'other_json_reads':[
            {'source_lines_before_patch':[72,207],'input':'one application JSON response or one CLI probe-body JSON value; strict json.loads remains appropriate'},
            {'source_lines_before_patch':[137,138,194],'input':'one saved inventory or image identity JSON object'},
            {'source_lines_before_patch':[129,145],'input':'kubectl get output for one object or one consolidated list; no multi-document create operation'},
        ]}
(out/'offline-replay.json').write_text(json.dumps(result,indent=2)+'\n')
(out/'decoded-crds.json').write_text(json.dumps(objects,indent=2)+'\n')
print(json.dumps({'patch':str(out/'operator-json.patch'),'patch_sha256':result['patch_sha256'],'decoded_crds':len(rows),'spec_hashes_match':True,'refusal_controls':len(controls),'application_check':check.returncode}))
