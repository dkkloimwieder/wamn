from collections import Counter
import gzip, hashlib, importlib.util, json, re, shutil, subprocess
from pathlib import Path
root=Path('/home/kaalin/dev/wamn')
out=root/'docs/perf/2026.09/consolidation-step2/run-002'
red=out/'reduction-001'
read=lambda p: json.loads(p.read_text())
run=read(out/'run.json'); source=run['source_head']
raw=read(red/'workspace-results.json'); value=read(red/'classified-failures-draft.json')
lines=(out/'workspace.log').read_text().splitlines()
before=read_map=json.loads(gzip.decompress((out/'source-before.json.gz').read_bytes()))
assert before==json.loads(gzip.decompress((out/'source-after.json.gz').read_bytes()))
assert hashlib.sha256((out/'workspace.log').read_bytes()).hexdigest()==run['workspace_log_sha256']
assert not raw['unresolved']
def write(name,data):
 with (out/name).open('x') as f: json.dump(data,f,indent=2); f.write('\n')
def excerpt(path,start,end):
 data=subprocess.check_output(['git','show',source+':'+path],cwd=root)
 assert hashlib.sha256(data).hexdigest()==before[path]['sha256']
 text=data.decode().splitlines()
 return {'path':path,'source':source,'sha256':hashlib.sha256(data).hexdigest(),'start_line':start,'end_line':end,'lines':[{'line':i,'text':text[i-1]} for i in range(start,end+1)]}
sources={
 'writer_scan':excerpt('tests/conformance/tests/state_ownership.rs',892,917),
 'unqualified_target':excerpt('tests/conformance/tests/state_ownership.rs',1477,1492),
 'app_statement':excerpt('apps/client_acme_receiving/command/approve_inspection/approve_inspection.sql',1,19),
}
for f in value['failures']:
 if f['classification']=='requires_current_cause_review':
  assert f['name']=='repository_state_ownership_manifest_is_complete'
  f.update(classification='app_move_expanded_static_writer_scan',required_inputs=[],reviewed_explanation='The apps root includes authored application SQL that the former components root did not scan. The static platform ownership table does not declare those application targets. This is a scope change in the retained scan, not an application execution failure.',source_evidence='tests/conformance/tests/state_ownership.rs:892',source_evidence_keys=list(sources))
 if f['name']=='router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle':
  starts=[i for i,line in enumerate(lines) if line.startswith('test '+f['name']+' ...')]
  assert len(starts)==2
  finish=next(i for i in range(starts[-1]+1,len(lines)) if re.match(r'^test .+? \.\.\. ',lines[i]))
  block=[{'log_line':i+1,'text':lines[i]} for i in range(starts[0],finish)]
  assert any('set WAMN_NATIVE_B_AUTH_PG_URL' in r['text'] for r in block)
  f.update(retained_reducer_raw_diagnostics=f['raw_diagnostics'],raw_diagnostics=block,diagnostic_start_log_line=starts[0]+1,diagnostic_end_log_line=finish,cause='WAMN_NATIVE_B_AUTH_PG_URL is absent. The nested authenticated native test exits 101 before live work.',required_inputs=['WAMN_NATIVE_B_AUTH_PG_URL'],source_evidence='crates/execution/host/src/router_driver/native_policy/tests/authenticated.rs:574',reviewed_explanation='The unchanged reducer keeps the same-named child diagnostic. The parent raw output states the missing PostgreSQL input.')
 if 'source_evidence' not in f:
  for r in f['raw_diagnostics']:
   match=re.search(r'panicked at (.+):(\d+):\d+:$',r['text'])
   if match: f['source_evidence']=match[1]+':'+match[2]; break
 assert f['classification']!='requires_current_cause_review'
value['classification_counts']=dict(Counter(f['classification'] for f in value['failures']))
assert len(value['failures'])==83
assert value['classification_counts']['app_move_expanded_static_writer_scan']==1
value.pop('manual_review_required')
value['unresolved_classifications']=[]
value['interpretation']='The run failed. All 83 failures remain failures. The 84 explicit skips did not execute their skipped work. No live inputs were armed.'
value['reviewed_source_excerpts']='reviewed-source-excerpts.json'
write('classified-failures.json',value); write('reviewed-source-excerpts.json',sources)
write('explicit-self-skips.json',value['explicit_self_skips'])
for name in ['workspace-results.json','baseline-comparison.json','step1-comparison.json','classification-inputs.json','test-case-delta-draft.json']:
 shutil.copyfile(red/name,out/name.replace('-draft',''))
shutil.copyfile(Path('/tmp/consolidation-stage2-classification-20260911/known-removals.json'),out/'known-test-removals.json')
# Reuse the retained comparator for the preceding run.
guidance=read(Path('/tmp/consolidation-step2-classification-guidance.json'))
path=Path(guidance['paths']['retained_comparator'])
spec=importlib.util.spec_from_file_location('run002_compare',path); compare=importlib.util.module_from_spec(spec); spec.loader.exec_module(compare)
reducer=compare.load(Path(guidance['paths']['retained_reducer']),'run002_reduce')
causes=compare.load(Path(guidance['paths']['retained_cause_normalizer']),'run002_causes')
previous=read(out.parent/'run-001/workspace-results.json')
comparison=compare.compare(raw,previous,reducer,causes)
assert len(comparison['absent_reference_failures'])==23
assert not comparison['added_failures']
assert len(comparison['changed_causes'])==2
comparison.update(source=source,reference_source=read(out.parent/'run-001/run.json')['source_head'])
write('previous-run-comparison.json',comparison)
# Match explicit skips by executable as well as their generic libtest description.
def skip_keys(result):
 targets={t['running_log_line']:t for t in result['test_targets']}
 return Counter((s['target_description'],re.sub(r'-[0-9a-f]+$','',Path(targets[s['target_running_log_line']]['executable']).name),s['name']) for s in result['explicit_self_skips']['entries'])
assert skip_keys(previous)==skip_keys(raw)
counts=raw['counts']; classes=value['classification_counts']
def cell(s):return str(s).replace('|','\\|').replace('\n','<br>')
def loglink(i):return f'[log {i}](workspace.log#L{i})'
text=f'''The second app-move workspace run failed on `{source}`. It reports 83 failed tests, 39 failed targets, and 84 explicit skips. It exited 101 after {run['elapsed_seconds']} seconds. All {len(before):,} tracked files outside Beads kept their bytes and modes. HEAD stayed unchanged. [Run](run.json), [source stability](source-stability.json), [full log](workspace.log).

The command is the retained workspace sweep. It uses Rust 1.98, debug builds, two jobs, and no armed live inputs. The same two schema-generation cases remain filtered. [Command](command.json), [environment names](environment-names.json).

The run reports 2,188 passes, which include 84 explicit skips, and six passing doctests. Subtracting known skips gives 2,104 reported passes. Silent early returns can remain undetected, so this is not an exact count of executed work.

Compared with the first app-move run, 23 failing cases now pass. The static writer case still fails after reaching a later assertion. The mounted-Secret diagnostic now matches the retained WMS prerequisite failure. [Previous comparison](previous-run-comparison.json).

Compared with Step 1, 82 failure identities and causes match. One additional failure remains because the apps directory also contains authored application SQL. The static platform writer scan now reads that SQL and requests a separate schema declaration. The old components root did not include those files. [Captured source](reviewed-source-excerpts.json), [Step 1 comparison](step1-comparison.json).

Two old failing cases and one old skipped case were deleted with native C. Those removals are not passing tests. The baseline comparison also records the earlier renamed app-build input. [Removal records](known-test-removals.json), [baseline comparison](baseline-comparison.json).

This run does not complete Step 2. The changed scan scope needs a separate correction. Every failure and explicit skip follows, with exact retained identifiers.

| Classification | Failed tests |
| --- | ---: |
'''
text+='\n'.join(f'| {cell(k.replace("_"," "))} | {v} |' for k,v in classes.items())
text+='\n\n| Package / target | Failed case | Classification | Actual cause | Evidence |\n| --- | --- | --- | --- | --- |\n'
for f in value['failures']:
 text+='| '+' | '.join([cell('`'+f['package']+'` `'+f['cargo_target']+'`'),cell('`'+f['name']+'`'),cell(f['classification'].replace('_',' ')),cell(f['cause']),loglink(f['diagnostic_start_log_line'])])+' |\n'
text+='\nThe unchanged reducer has no unresolved result. Its same-named native child hides the parent diagnostic in the reduced record. The classification retains both raw occurrences and the missing database input. [Original reduction](reduction-001/workspace-results.json).\n\nAll 84 explicit skips have the same names and target executables as the previous run. These cases did not execute their skipped work. [Skip records](explicit-self-skips.json).\n\n| Target executable / description | Case | Explicit skip message | Evidence |\n| --- | --- | --- | --- |\n'
for s in value['explicit_self_skips']['entries']:
 exe=re.sub(r'-[0-9a-f]+$','',Path(s['target_executable']).name)
 text+='| '+' | '.join([cell('`'+exe+'` / `'+s['target_description']+'`'),cell('`'+s['name']+'`'),cell(s['message']),loglink(s['diagnostic_log_line'])])+' |\n'
(out/'report.md').write_text(text)
shutil.copyfile(Path(__file__),out/'finalize-classification.py')
shutil.copyfile(Path('/tmp/consolidation-stage2-classification-20260911/reduce.py'),out/'reduce-current.py')
write('classification-summary.json',dict(source=source,exit_code=raw['exit_code'],counts=counts,classification_counts=classes,added_failure_identities_vs_step1=1,removed_failure_identities_vs_step1=2,unchanged_causes_vs_step1=82,explicit_self_skips=84,unresolved_classifications=[],tracked_files=len(before),source_stable=True,log_sha256=run['workspace_log_sha256'],report_sha256=hashlib.sha256((out/'report.md').read_bytes()).hexdigest()))
print(json.dumps({'source':source,'failures':len(value['failures']),'skips':84,'classes':classes},indent=2))
