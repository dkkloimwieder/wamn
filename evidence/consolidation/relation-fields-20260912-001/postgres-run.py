from pathlib import Path
import datetime,json,os,subprocess,sys,time,urllib.parse
out=Path(__file__).resolve().parent;root=out.parents[4]
url=urllib.parse.urlunsplit(('postgresql',urllib.parse.quote(os.environ['PGUSER'],safe='')+':'+urllib.parse.quote(os.environ['PGPASSWORD'],safe='')+'@127.0.0.1:'+os.environ['PGPORT'],'/postgres','',''))
env=os.environ.copy();env['WAMN_RECEIVING_PG_URL']=url
argv=[sys.argv[1],'receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns','--exact','--ignored','--nocapture']
start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
with (out/'postgres.log').open('wb') as log:
 os.chmod(log.name,0o600);result=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
record={'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':result.returncode,'postgres':{'version':subprocess.check_output(['psql','-X','-Atqc','SHOW server_version'],text=True).strip(),'configuration_root':os.environ['PG_CLUSTER_CONF_ROOT'],'port':os.environ['PGPORT']}}
(out/'postgres-result.json').write_text(json.dumps(record,indent=2)+'\n')
raise SystemExit(result.returncode)
