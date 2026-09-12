from pathlib import Path
import re,subprocess,json,hashlib,difflib
root=Path('/home/kaalin/.cache/wamn-lanes/consolidation-generator-20260912')
renames={
'exact_ingress_refusals_prove_no_dispatch_even_for_composed_routes':'exact_ingress_refusals_report_no_dispatch_even_for_composed_routes',
'malformed_error_and_mixed_envelopes_cannot_prove_commitment':'malformed_error_and_mixed_envelopes_cannot_report_commitment',
'multi_export_admission_proves_global_union_and_preserves_attachment':'multi_export_admission_checks_global_union_and_preserves_attachment',
'disposable_postgres_proves_freshness_cleanup_and_confinement':'disposable_postgres_checks_freshness_cleanup_and_confinement',
'trigger_event_declarations_are_not_inventoried_as_writes':'trigger_event_declarations_are_not_listed_as_writes',
'interpolated_set_clause_still_inventories_the_update':'interpolated_set_clause_still_lists_the_update'}
def without_comments(source):
 i=0;out=[];comment_lines=set();line=1
 while i<len(source):
  if source.startswith('//',i):
   j=source.find('\n',i)
   if j<0:j=len(source)
   comment_lines.add(line);i=j;continue
  if source.startswith('/*',i):
   j=i+2;depth=1
   while depth:
    assert j<len(source)
    if source.startswith('/*',j):depth+=1;j+=2
    elif source.startswith('*/',j):depth-=1;j+=2
    else:j+=1
   block=source[i:j];line+=block.count('\n');out.append('\n'*block.count('\n'));i=j;continue
  raw=re.match(r'(?:b|c)?r(\#*)"',source[i:])
  if raw:
   end='"'+raw.group(1);j=source.find(end,i+raw.end());assert j>=0
   j+=len(end);out.append(source[i:j]);line+=source[i:j].count('\n');i=j;continue
  if source[i]=='"':
   j=i+1
   while j<len(source):
    if source[j]=='\\':j+=2
    elif source[j]=='"':j+=1;break
    else:j+=1
   out.append(source[i:j]);line+=source[i:j].count('\n');i=j;continue
  if source[i]=="'":
   char=re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'",source[i:])
   if char:out.append(char.group());i+=char.end();continue
  if source[i]=='\n':line+=1
  out.append(source[i]);i+=1
 return ''.join(out),comment_lines
files=subprocess.check_output(['git','diff','--name-only'],cwd=root,text=True).splitlines()
comment_changes=0;tokens=0;details=[]
for p in files:
 before=subprocess.check_output(['git','show','HEAD:'+p],cwd=root).decode()
 after=(root/p).read_text()
 assert before.count('\n')==after.count('\n'),p
 a,comments=without_comments(before);b,_=without_comments(after)
 for old,new in renames.items():
  if 'fn '+new+'(' in b:
   assert b.count('fn '+new+'(')==1
   b=b.replace('fn '+new+'(','fn '+old+'(');tokens+=1
 assert a==b,('source token or literal changed',p)
 old_lines=before.splitlines();new_lines=after.splitlines();n=0
 for line,(old,new) in enumerate(zip(old_lines,new_lines),1):
  if old==new:continue
  if line in comments:
   assert old.lstrip().startswith('//') and new.lstrip().startswith('//'),(p,line)
   assert re.findall(r'`+[^`]*`+',old)==re.findall(r'`+[^`]*`+',new),(p,line,'quoted text')
   n+=1
  else:assert any(old.replace(a,b)==new for a,b in renames.items()),(p,line)
 comment_changes+=n;details.append({'path':p,'comment_lines':n})
assert tokens==6,tokens
assert not subprocess.check_output(['git','diff','--summary'],cwd=root,text=True)
print(json.dumps({'files':len(files),'comment_lines_changed':comment_changes,'test_name_replacements':tokens,'all_other_source_tokens_and_literals_identical':True,'line_counts_unchanged':True,'backtick_quoted_text_unchanged':True,'git_modes_unchanged':True},indent=2))
for p in files:
 s=(root/p).read_text()
 if any(x in s for x in ['JsonSchema','Args','ValueEnum','Subcommand','Parser']):
  print('public_output_review',p)
