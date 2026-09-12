import datetime,hashlib,json,os,pathlib,subprocess,time
out=pathlib.Path(__file__).resolve().parent;root=out.parents[4]
paths=['crates/schema/control/src/sql.rs','deploy/sql/catalog-schema.sql']
source={'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'files':{n:{'sha256':hashlib.sha256((root/n).read_bytes()).hexdigest(),'mode':oct((root/n).stat().st_mode&0o777)} for n in paths}}
(out/'source.json').write_text(json.dumps(source,indent=2)+'\n')
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
argv=['cargo','+1.98.0','test','--locked','--offline','-p','wamn-schema-control','--lib','sql::tests','--','--nocapture']
start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
with (out/'cargo.log').open('wb') as log:r=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
unchanged=all(hashlib.sha256((root/n).read_bytes()).hexdigest()==source['files'][n]['sha256'] for n in paths)
record={'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':r.returncode,'source_unchanged':unchanged,'environment':{'RUSTC_WRAPPER':'','CARGO_BUILD_JOBS':'2'}}
(out/'result.json').write_text(json.dumps(record,indent=2)+'\n')
raise SystemExit(r.returncode)
