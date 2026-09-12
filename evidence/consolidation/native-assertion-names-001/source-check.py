import ast,collections,hashlib,io,json,pathlib,re,subprocess,tokenize
root=pathlib.Path('/home/kaalin/.cache/wamn-lanes/consolidation-postgres-claims-20260912')
evidence=pathlib.Path(__file__).resolve().parent
record=json.loads((evidence/'rename-map.json').read_text())
parent='c99055b483e32ac13436122356777f8b6b2074d5'
def tokens(source):
    result = []
    comments = []
    literals = []
    i = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
            continue
        start = i
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
            comments.append(source[start:i])
            continue
        if source.startswith('/*', i):
            depth = 1
            i += 2
            while depth:
                assert i < len(source)
                if source.startswith('/*', i):
                    depth += 1
                    i += 2
                elif source.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            comments.append(source[start:i])
            continue
        raw = re.match('(?:b|c)?r(#+)?"', source[i:])
        if raw:
            close = '"' + (raw[1] or '')
            end = source.find(close, i + len(raw[0]))
            assert end >= 0
            i = end + len(close)
            literals.append(source[start:i])
            result.append(source[start:i])
            continue
        string = re.match('(?:b|c)?"', source[i:])
        if string:
            i += len(string[0])
            while source[i] != '"':
                if source[i] == '\\':
                    i += 2
                else:
                    i += 1
                assert i < len(source)
            i += 1
            literals.append(source[start:i])
            result.append(source[start:i])
            continue
        char = re.match("b?'(?:\\\\.|[^'\\\\\\n])'", source[i:])
        if char:
            i += len(char[0])
            literals.append(char[0])
            result.append(char[0])
            continue
        word = re.match('[A-Za-z_][A-Za-z_0-9]*|[0-9][A-Za-z_0-9]*', source[i:])
        if word:
            i += len(word[0])
        else:
            i += 1
        result.append(source[start:i])
    return (result, comments, literals)
rows=[];literal_count=0
for rel,names in record['files'].items():
 old=subprocess.check_output(['git','show',f'{parent}:{rel}'],cwd=root).decode();new=(root/rel).read_text()
 ot,oc,ol=tokens(old);nt,nc,nl=tokens(new)
 expected=[names.get(t,t) for t in ot];expected_literals=ol.copy()
 if rel=='services/ctl/src/publish_release.rs':
  ex=record['literal_exception'];expected=[ex['new'] if t==ex['old'] else t for t in expected];expected_literals=[ex['new'] if t==ex['old'] else t for t in ol]
 if rel.endswith('/effective_release_live.rs'):
  # Rustfmt only reorders the imported renamed function in this existing block.
  ob=re.search(r'use super::\{.*?\};',old,re.S)[0];nb=re.search(r'use super::\{.*?\};',new,re.S)[0]
  obt=tokens(ob)[0];nbt=tokens(nb)[0];mapped=[names.get(t,t) for t in obt]
  assert collections.Counter(mapped)==collections.Counter(nbt)
  start=next(i for i in range(len(expected)) if expected[i:i+len(mapped)]==mapped)
  expected[start:start+len(mapped)]=nbt
 assert expected==nt,rel
 assert expected_literals==nl,rel
 literal_count+=len(ol)
 rows.append({'path':rel,'before_sha256':hashlib.sha256(old.encode()).hexdigest(),'after_sha256':hashlib.sha256(new.encode()).hexdigest(),'literal_count':len(ol),'mapped_executable_tokens_equal':True,'literal_bytes_equal':ol==nl,'mode':oct((root/rel).stat().st_mode&0o777),'comments_changed':oc!=nc})
rel=record['python_file']['path'];old=subprocess.check_output(['git','show',f'{parent}:{rel}'],cwd=root).decode();new=(root/rel).read_text()
expected=[]
for t in tokenize.generate_tokens(io.StringIO(old).readline):expected.append((t.type,'assert_operator' if t.type==tokenize.NAME and t.string=='prove' else t.string))
assert expected==[(t.type,t.string) for t in tokenize.generate_tokens(io.StringIO(new).readline)]
compile(new,rel,'exec')
rows.append({'path':rel,'before_sha256':hashlib.sha256(old.encode()).hexdigest(),'after_sha256':hashlib.sha256(new.encode()).hexdigest(),'mapped_tokens_equal':True,'compiled_without_execution':True,'mode':oct((root/rel).stat().st_mode&0o777)})
changes=subprocess.check_output(['git','diff','--name-only',parent,'HEAD'],cwd=root,text=True).splitlines()
assert sorted(changes)==sorted(r['path'] for r in rows)
result={'parent':parent,'files':rows,'file_count':len(rows),'rust_literal_count':literal_count,'literal_exception':record['literal_exception'],'test_name_changes':{k:v for k,v in record['files']['services/ctl/src/publish_release.rs'].items() if k.startswith('a_dependency_')},'module_package_target_changes':[]}
(evidence/'source-check.json').write_text(json.dumps(result,indent=2)+'\n')
(evidence/'rename-map.json').write_text(json.dumps(record,indent=2)+'\n')
print(json.dumps({'files':len(rows),'rust_literals':literal_count,'tokens_match':True,'literal_exceptions':1,'test_names_changed':2,'python_source_compiles':True}))
