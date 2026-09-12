from pathlib import Path
import datetime,hashlib,json,os,subprocess,time
root=Path('/home/kaalin/.cache/wamn-lanes/consolidation-sql-package-20260912');out=root/'docs/perf/2026.09/consolidation-step4/relation-fields-001';out.mkdir()
env={k:v for k,v in os.environ.items() if not (k.startswith(('WAMN_','PG','OTEL_')) or k in ('DATABASE_URL','DB_URL','CARGO_TARGET_DIR'))};env.update(RUSTC_WRAPPER='',CARGO_BUILD_JOBS='2')
records=[]
def run(argv,name):
 start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with (out/name).open('wb') as log:
  os.chmod(log.name,0o600);r=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
 records.append({'argv':argv,'cwd':str(root),'started':stamp,'elapsed_seconds':time.monotonic()-start,'exit':r.returncode})
 (out/'baseline-commands.json').write_text(json.dumps(records,indent=2)+'\n');assert r.returncode==0
run(['cargo','+1.98.0','build','--locked','--offline','-p','wamn-schema-generator','--lib','--message-format=json-render-diagnostics'],'baseline-cargo.log')
lib=None
for line in (out/'baseline-cargo.log').read_text().splitlines():
 try:a=json.loads(line)
 except ValueError:continue
 if a.get('reason')=='compiler-artifact' and a['target']['name']=='wamn_schema_generator':lib=next(p for p in a['filenames'] if p.endswith('.rlib'))
assert lib
source='''use wamn_schema_generator::{DataAccessOverlay, DataAccessRelationInventory, derive_data_access_overlay_from_inventory};
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 3);
    let manifest = std::fs::read(&args[0]).unwrap();
    let bytes = std::fs::read(&args[1]).unwrap();
    let overlay = DataAccessOverlay::from_slice(&bytes).unwrap();
    let relation_fields = overlay.relations().iter().map(|relation| DataAccessRelationInventory::new(relation.schema(), relation.table(), relation.all_fields().to_vec())).collect::<Vec<_>>();
    let regenerated = derive_data_access_overlay_from_inventory(&relation_fields, &manifest).unwrap().canonical_bytes();
    std::fs::write(&args[2], &regenerated).unwrap();
    assert_eq!(bytes, regenerated);
}
'''
probe=out/'baseline-probe.rs';probe.write_text(source);binary=Path('/tmp/consolidation-relation-fields-before')
run(['rustc','+1.98.0','--edition=2024',str(probe),'-o',str(binary),'-L','dependency='+str(root/'target/debug/deps'),'--extern','wamn_schema_generator='+lib],'baseline-probe-build.log')
inputs={}
for app in ['wamn_receiving','wamn_wms','client_acme_receiving']:
 manifest=root/'apps'/app/'wamn.json';overlay=manifest.parent/'generated/platform-policy/data-access.json';output=out/f'{app}-before.json'
 run([str(binary),str(manifest),str(overlay),str(output)],f'{app}-before.log')
 inputs[app]={'manifest':{'path':str(manifest.relative_to(root)),'sha256':hashlib.sha256(manifest.read_bytes()).hexdigest()},'overlay':{'path':str(overlay.relative_to(root)),'sha256':hashlib.sha256(overlay.read_bytes()).hexdigest(),'bytes':overlay.stat().st_size},'regenerated_sha256':hashlib.sha256(output.read_bytes()).hexdigest()}
(out/'baseline-source.json').write_text(json.dumps({'source':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'inputs':inputs},indent=2)+'\n')
print(json.dumps({'baseline_overlays_exact':len(inputs),'commands':len(records)}))
