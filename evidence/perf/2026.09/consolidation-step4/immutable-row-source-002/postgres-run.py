import datetime, hashlib, json, os, pathlib, subprocess, sys, time, urllib.parse
out=pathlib.Path(__file__).resolve().parent
root=out.parents[4]

def capture(argv, log, env):
    start=time.monotonic(); stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
    with log.open('wb') as stream:
        os.chmod(log,0o600)
        result=subprocess.run(argv,cwd=root,env=env,stdout=stream,stderr=subprocess.STDOUT)
    return {'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':result.returncode}

if len(sys.argv)>1 and sys.argv[1]=='inside':
    case=sys.argv[2]; binary=sys.argv[3]; args=sys.argv[4:]
    # pg_virtualenv owns this temporary cluster and supplies its private password.
    url=urllib.parse.urlunsplit(('postgresql',urllib.parse.quote(os.environ['PGUSER'],safe='')+':'+urllib.parse.quote(os.environ['PGPASSWORD'],safe='')+'@127.0.0.1:'+os.environ['PGPORT'],'/postgres','',''))
    subprocess.run(['psql','-X','-v','ON_ERROR_STOP=1','-q'],input="DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN CREATE ROLE postgres NOLOGIN SUPERUSER; END IF; END $$;",text=True,check=True)
    env=os.environ.copy();env['WAMN_CTL_PG_URL']=url;env['WAMN_CONTROL_PORTABLE_PG_URL']=url
    record=capture([binary,*args],out/(case+'.log'),env)
    record['postgres']={'version':subprocess.check_output(['psql','-X','-Atqc','SHOW server_version'],text=True).strip(),'configuration_root':os.environ['PG_CLUSTER_CONF_ROOT'],'port':os.environ['PGPORT']}
    (out/(case+'.json')).write_text(json.dumps(record,indent=2)+'\n')
    raise SystemExit(record['exit'])

artifacts={}
for log in ('command-2.log','command-3.log'):
    for line in (out/log).read_text().splitlines():
        try: value=json.loads(line)
        except ValueError: continue
        if value.get('reason')=='compiler-artifact' and value.get('executable') and value['profile']['test']:
            artifacts[value['target']['name']]=value['executable']
(out/'artifacts.json').write_text(json.dumps({k:{'path':v,'sha256':hashlib.sha256(pathlib.Path(v).read_bytes()).hexdigest(),'bytes':pathlib.Path(v).stat().st_size} for k,v in artifacts.items()},indent=2)+'\n')
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))}
records=[]
for case,target,args in [
 ('registration','wamn_ctl',['apply_package::registration_tests::registration_serializes_replay_conflicts_successors_and_rollback','--exact','--ignored','--nocapture']),
 ('publish-release','publish_release_live',['--nocapture']),
 ('portable-store','control_portable_store',['control_','--nocapture']),
]:
    argv=([artifacts[target],*args] if case=='ordinary-ctl' else ['pg_virtualenv','-t','-v','18',sys.executable,str(__file__),'inside',case,artifacts[target],*args])
    result=capture(argv,out/(case+'-controller.log'),env)
    detail=out/(case+'.json')
    if detail.exists():
        data=json.loads(detail.read_text());result['temporary_cluster_removed']=not pathlib.Path(data['postgres']['configuration_root']).exists()
    records.append(result)
    (out/'postgres-results.json').write_text(json.dumps(records,indent=2)+'\n')
    # Preserve all requested case outcomes, including failures.
raise SystemExit(any(r['exit'] for r in records))
