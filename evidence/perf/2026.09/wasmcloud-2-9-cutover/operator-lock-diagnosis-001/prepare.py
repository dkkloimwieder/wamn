from pathlib import Path
import hashlib,json,datetime
up=Path('/home/kaalin/.cache/wamn-lanes/upstream-wasmcloud-2-9-20260909')
main=Path('/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover')
lane=Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909/docs/perf/2026.09/wasmcloud-2-9-cutover')
def h(p):
 p=Path(p);return {'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
prior=main/'operator-timeout-diagnosis-001/diagnosis.json'
x=json.loads(prior.read_text())
url='https://raw.githubusercontent.com/nats-io/nats.go/v1.53.1/nats.go'
# These are explicitly labelled web-extraction positions; no unverified raw
# file line-number conversion or raw-file digest is invented.
rows=[
 {'symbol':'Conn.mu; IsClosed; isClosed','web_lines_zero_based':[597,5803,5804,5805,5806,5988,5989],'fact':'IsClosed takes the connection RWMutex read lock; no timeout or context.'},
 {'symbol':'doReconnect','web_lines_zero_based':[3074,3181,3238,3248,3263,3281,3284,3311],'fact':'Writer lock spans createConn, handshake, subscription replay and pending flush. Reconnect sleep releases it.'},
 {'symbol':'createConn; Options.Connect','web_lines_zero_based':[1828,1830,2295,2296,2311,2312,2320,2321],'fact':'Hostname lookup precedes timed TCP dialing. The explicit LookupHost has no NATS Timeout bound; dial timeout divides across resolved addresses.'},
 {'symbol':'processConnectInit; timeoutWriter.Write','web_lines_zero_based':[2670,2672,2673,2692,6413,6414,6415,6416],'fact':'Handshake installs a fresh Timeout deadline. Writer separately sets and clears its write deadline; do not infer a whole-lock 5-second ceiling.'},
 {'symbol':'flushReconnectPendingItems; natsWriter; newWriter','web_lines_zero_based':[2144,2146,2147,2184,2190,2205,2209,3051,3052,3284],'fact':'Pending replay can write under lock; each timeoutWriter write uses FlusherTimeout. Ordinary pending-mode flush does no socket write.'},
 {'symbol':'FlushTimeout; doReconnect final Flush','web_lines_zero_based':[3311,3314,5528,5532,5568,5573,5579,5580,5582,5589],'fact':'Final Flush releases the lock while awaiting PONG, but sendPing writes under lock; its timer starts after lock acquisition.'},
]
# Short exact excerpts, with hashes of these retained bytes only.
snips=[('isclosed','nc.mu.RLock()\n\tdefer nc.mu.RUnlock()\n\treturn nc.isClosed()\n',[5804,5805,5806]),('reconnect-dial','err = nc.createConn()\n',[3248]),('reconnect-pending-flush','nc.err = nc.flushReconnectPendingItems()\n',[3284])]
sniprefs=[]
for name,s,lines in snips:
 p=Path('/tmp/wamn-cutover-operator-nats-lock-prepared')/(name+'.excerpt.txt');p.write_text(s);sniprefs.append({**h(p),'source_url':url,'web_lines_zero_based':lines,'hash_scope':'Retained excerpt bytes, not complete upstream file'})
deployment=main/'live-receiving-006/journey/operator-deployment.json'
d=json.loads(deployment.read_text());d=d.get('items',[d])[0];container=d['spec']['template']['spec']['containers'][0]
report={
 'schema':'wamn-operator-nats-lock-diagnosis/v1','prepared_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
 'scope':'Bounded source diagnosis only. Prior diagnosis read first. No Cargo/Go commands, dependency downloads, builds, tests, Docker, live probes, source edits, Beads writes, acceptance changes or owner-question changes.',
 'conclusion':{'lock_hypothesis':'Not ruled out: concrete source path exists.','measured_cause':'Still unknown. Receiving006 establishes kubelet HTTP-header timeout and clean shutdown, not lock contention or terminal NATS closure.','next_step':'For any later authorized rerun, retain paired health/ready response timing through a longer observational deadline; definitive lock attribution still needs a contemporaneous Go stack or lock trace.'},
 'previous_diagnosis':h(prior),
 'local_upstream_identity':{'commit':'68ebece9c537f8bb4b5c9999f274ec68d60f35a9','inputs':[h(up/p) for p in ['runtime-operator/go.mod','runtime-operator/go.sum','runtime-operator/cmd/main.go','runtime-operator/pkg/wasmbus/nats.go','charts/runtime-operator/Chart.yaml','charts/runtime-operator/values.yaml','charts/runtime-operator/templates/operator/deployment.yaml']]},
 'nats_dependency_identity':{'module':'github.com/nats-io/nats.go','version':'v1.53.1','module_sum':'h1:Otsq3uLc/kLdjmkNHkXH0jBqwUquwdKFoe3fq6/3/Xo=','go_mod_sum':'h1:26HypzazeOkyO3/mqd1zZd53STJN0EjCYF9Uy2ZOBno=','pin_source':'runtime-operator/go.mod:20; runtime-operator/go.sum:288-289; no replace directive','local_module_cache':'No matching source found in inspected home Go/cache/tmp paths.','primary_source_url':url,'release_url':'https://github.com/nats-io/nats.go/releases/tag/v1.53.1','release_commit':'db1375fcffae2eb0b4ced1b7bad4d47c4447e4ac','release_commit_basis':'The primary release page links this full commit. Raw tag source was readable; commit-addressed raw source and GitHub commit/API reads returned fetch errors. No module download/hash verification or executable build-info verification was performed.','line_convention':'NATS line positions below are explicitly the web reader\'s zero-based extraction positions, not asserted GitHub/raw-file line numbers. Symbol names permit rederivation. Local repository line citations are ordinary one-based lines.','complete_nats_go_sha256':None,'complete_nats_go_sha256_reason':'Full raw bytes were not retained; module h1 and linked release commit are identity evidence, not a fabricated file SHA.'},
 'lock_path':rows,'retained_short_excerpts':sniprefs,
 'operator_health_path':{'source':'runtime-operator/cmd/main.go:319-354','healthz':'healthz.Ping plus natsLivenessCheck; checker discards HTTP request/context, calls nc.IsClosed, and reports failure only after true is returned.','readyz':'healthz.Ping only.','implication':'Even an intended healthy reconnect result cannot be returned before IsClosed finishes. A timeout does not imply that the function returned true.','options_source':'runtime-operator/pkg/wasmbus/nats.go:57-71','connection_timeout_seconds':5,'flusher_write_timeout_seconds':30,'reconnect_wait_seconds':1,'reconnect_jitter_seconds':{'plain':0.1,'TLS':1},'max_reconnects':-1,'initial_connect_window_seconds':60,'initial_window_scope':'Before initial connection; not a deadline for the existing Receiving006 connection outage.'},
 'deployed_probe_evidence':{**h(deployment),'args':container['args'],'liveness':container['livenessProbe'],'readiness':container['readinessProbe'],'startup':container['startupProbe'],'comparison':'Configured NATS dial timeout5s and flusher timeout30s both exceed measured Kubernetes timeoutSeconds1. This is a configuration/source comparison, not an observed stall duration.','observed_transition_inputs':[{**h(main/'live-receiving-006/journey/operator-recovery'/p)} for p in ['0328-operator-transition-3-previous-log.stdout','0329-operator-transition-3-events.stdout','operator-transition-3-state.json']]},
 'distributed_chart_timeout_surface':{
  'chart':'oci://ghcr.io/wasmcloud/charts/runtime-operator','version':'2.9.0','oci_manifest_digest':'sha256:d70b240cfc3c745f306fc6ebecebff4370e6c8c0568b55c1d02b1eda1716fd17','package_sha256':'98c4e5e1bf19d4549fb2ca72bf8e7874d2e068a05e254370e75dce6f98329109',
  'identity_receipts':[h(lane/'deployment-001/distributed-chart.json'),h(lane/'deployment-crds-001/distributed-chart-identities.json')],
  'ordinary_timeout_value_path':None,'template_source':'charts/runtime-operator/templates/operator/deployment.yaml:85-116','values_source':'charts/runtime-operator/values.yaml:153-172','actual_timeout_seconds':1,
  'supported_values':{'operator.probes.startup':{'enabled':True,'periodSeconds':5,'failureThreshold':24},'operator.probes.liveness':{'enabled':True,'initialDelaySeconds':15,'periodSeconds':20,'failureThreshold':3},'operator.probes.readiness':{'enabled':True,'initialDelaySeconds':5,'periodSeconds':10,'failureThreshold':3}},
  'ignored_override':'operator.probes.liveness.timeoutSeconds is not read by the template; adding it alone does not change the rendered Deployment. The same omission applies to startup/readiness.',
  'verification_scope':'Read tagged source templates and retained distributed-chart identities plus actual deployed fields. The temporary downloaded chart archive was not found and was not pulled again; no new render/override experiment was run.',
  'possible_remedy_scope':'If the owner chooses timeout tuning, the actual Kubernetes field is spec.template.spec.containers[name=runtime-operator].livenessProbe.timeoutSeconds. The 2.9 chart has no ordinary value for it; any upstream remedy or separately managed manifest transformation needs an explicit owner disposition. No timeout value, live patch, disablement, or acceptance change is proposed.'},
 'later_authorized_rerun_diagnostic':{
  'integration':'Reuse tools/receiving-operator-recovery-run owned Pod/node identities and existing diagnostic snapshots. Preserve its failure classification.',
  'trigger':'At the first retained 1-second liveness/header timeout, sample both endpoints concurrently from the exact owned kind node to the exact operator Pod IP.',
  'argv_templates':[['docker','exec','{exact_owned_kind_node}','curl','--silent','--show-error','--connect-timeout','1','--max-time','7','--write-out','\\nhttp=%{http_code} connect=%{time_connect} starttransfer=%{time_starttransfer} total=%{time_total}\\n','http://{operator_pod_ip}:8082/'+p] for p in ['healthz','readyz']],
  'why_seven_seconds':'An observational request may retain a delayed response beyond the configured five-second dial budget; it leaves the deployed one-second probe untouched. Seven seconds is not asserted sufficient for DNS or thirty-second writes.',
  'retain':'Absolute start/end timestamps, response body/status, connect/first-byte/total timing, curl exit, exact Pod UID/container ID, existing transition logs/events, and existing resource diagnostics; raw output private. Stop owned observation children at their finite deadline.',
  'interpretation':'Health delayed beyond1s while readiness stays prompt would support endpoint-specific delay. Simultaneous delays leave scheduling/transport explanations open. Neither pattern alone identifies nc.mu as the measured cause.',
  'definitive_missing_diagnostic':'A contemporaneous goroutine stack showing the health checker blocked in sync.RWMutex.RLock and reconnect/write work holding that same connection lock. No pprof listener/flag is exposed in current operator main.go. Do not claim a stack capture command exists; SIGQUIT would terminate the process and require a separate diagnostic disposition, not unchanged-process continuity.'},
 'validation':'Parsed retained JSON; hashed cited local source/evidence and exact short remote excerpts; followed primary pinned-version NATS source. No dynamic reproduction or causal verdict.'}
p=Path('/tmp/wamn-cutover-operator-nats-lock-prepared/diagnosis.json');p.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(h(p)));print('bytes='+str(p.stat().st_size))
