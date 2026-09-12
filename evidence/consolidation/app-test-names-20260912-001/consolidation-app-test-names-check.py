from pathlib import Path
import json,re,subprocess,hashlib
w=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912');o=Path('/home/kaalin/dev/wamn/evidence/consolidation/app-test-names-20260912-001');o.mkdir(mode=0o700)
paths=json.loads(Path('/tmp/consolidation-app-test-names-files.json').read_text())
pat=re.compile(r'(?P<literal>r(?P<hashes>#{0,255})".*?"(?P=hashes)|"(?:\\.|[^"\\])*")|(?P<comment>//[^\n]*|/\*.*?\*/)|(?P<identifier>\b[A-Za-z_][A-Za-z_0-9]*\b)|(?P<other>[^\s])',re.S)
def tokens(s):return [(m.lastgroup,m[0]) for m in pat.finditer(s) if m.lastgroup!='comment']
rows=[]
for p,names in paths.items():
 old=subprocess.check_output(['git','show','HEAD:'+p],cwd=w,text=True);new=(w/p).read_text();attrs=[]
 if p.endswith('/startup_burst_live.rs'):
  attribute='    #[serde(rename = "proof_id")]\n';assert new.count(attribute)==1;new=new.replace(attribute,'');attrs=[attribute.strip()]
 a=tokens(old);b=tokens(new);assert len(a)==len(b),(p,len(a),len(b));literals=[];exceptions=[]
 for (k,x),(l,y) in zip(a,b):
  assert k==l
  if k=='identifier':assert y==names.get(x,x),(p,x,y)
  elif k=='literal':
   if x!=y:
    assert p.endswith('/dev.rs') and y==x.replace('{receipts:?}','{results:?}'),(p,x,y)
    exceptions.append({'before':x,'after':y})
   else:literals.append(x)
  else:assert x==y,(p,x,y)
 rows.append({'path':p,'code_unchanged_except_declared_names_and_serde_attribute':True,'unchanged_literals':len(literals),'unchanged_literals_sha256':hashlib.sha256('\0'.join(literals).encode()).hexdigest(),'literal_template_exceptions':exceptions,'added_attributes':attrs})
(o/'source-comparison.json').write_text(json.dumps({'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=w,text=True).strip(),'files':rows},indent=2)+'\n')
print(json.dumps({'files':len(rows),'unchanged_literals':sum(x['unchanged_literals'] for x in rows),'template_exceptions':sum(len(x['literal_template_exceptions']) for x in rows),'serde_attributes':sum(len(x['added_attributes']) for x in rows)}))
