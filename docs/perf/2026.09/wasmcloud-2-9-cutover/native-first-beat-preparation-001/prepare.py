from pathlib import Path
import difflib, hashlib, json, subprocess
repo=Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
out=Path('/tmp/wamn-cutover-first-beat-prepared')
path='services/host/tests/native_lifecycle_live.rs'
source=(repo/path).read_text()
helper='''// Exercise a real native command-task failure through the public WAMN
// lifecycle helpers. This case runs in the test process, not the host child.
async fn prove_missing_first_beat(nats_address: SocketAddr, evidence: &Path) -> anyhow::Result<()> {
    use std::sync::Arc;

    use wash_runtime::host::probes::{Liveness, ProbeState};
    use wash_runtime::washlet::{ClusterHostBuilder, liveness_silence};
    use wamn_runtime::lifecycle::{bounded_cleanup, watch_liveness};

    let client = timeout(REQUEST_BUDGET, async_nats::connect(nats_address.to_string()))
        .await
        .context("connect the first-beat proof client within its budget")??;
    timeout(REQUEST_BUDGET, client.flush())
        .await
        .context("flush the connected first-beat proof client")??;
    timeout(REQUEST_BUDGET, client.drain())
        .await
        .context("request the owned client drain")??;
    // drain() only queues closure. Observe a failed valid subscription before
    // giving this client to the native host, so the first-beat failure is real.
    let closed = timeout(REQUEST_BUDGET, async {
        loop {
            match client.subscribe("lifecycle.first-beat.closed").await {
                Ok(subscription) => drop(subscription),
                Err(error) => break error,
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("drained client still accepted subscription commands")?;
    ensure!(
        closed.kind() == async_nats::SubscribeErrorKind::Other,
        "the closed-client control failed for a reason other than command delivery"
    );

    // A short public heartbeat interval keeps this proof bounded without a
    // zero silence budget or synthetic beat. The deployment default is 15s.
    let builder = ClusterHostBuilder::default()
        .with_host_group("lifecycle-proof")
        .with_nats_client(Arc::new(client))
        .with_heartbeat_interval(Duration::from_millis(100));
    let silence_budget = liveness_silence(builder.heartbeat_interval());
    let liveness = Liveness::new(silence_budget);
    let probes = ProbeState::default().with_liveness(Arc::clone(&liveness));
    // No ingress, plugins or workloads: a subscribe failure skips Host::stop.
    let native = builder.with_liveness(Arc::clone(&liveness)).build()?;
    let (host, cleanup) = timeout(REQUEST_BUDGET, native.start())
        .await
        .context("native start did not return before its first-beat proof budget")??;
    let started = Instant::now();
    let observed = timeout(
        silence_budget + REQUEST_BUDGET,
        watch_liveness(&liveness, &probes, silence_budget),
    )
    .await;
    let observation_time = started.elapsed();
    let never_beat = liveness.silence().is_none();
    // Poll cleanup only after the observer settles, even on its timeout path.
    probes.drain();
    let cleanup_started = Instant::now();
    let cleaned = bounded_cleanup(REQUEST_BUDGET, cleanup).await;
    let cleanup_time = cleanup_started.elapsed();
    drop(host);

    let error = observed
        .context("the failed native task was not detected within its silence bound")?;
    ensure!(
        error.to_string().contains("first liveness beat") && never_beat,
        "the native task did not fail before its first beat: {error:#}"
    );
    ensure!(
        observation_time >= silence_budget && observation_time < silence_budget + REQUEST_BUDGET,
        "first-beat observation did not preserve its configured silence bound"
    );
    ensure!(cleanup_time < REQUEST_BUDGET, "native cleanup exceeded its budget");
    let error = cleaned
        .err()
        .context("native subscription failure returned successful cleanup")?;
    let subscription = error
        .downcast_ref::<async_nats::SubscribeError>()
        .context("cleanup did not retrieve the native subscription error")?;
    ensure!(
        subscription.kind() == async_nats::SubscribeErrorKind::Other
            && error.to_string() == "failed to subscribe for API requests",
        "cleanup returned a different native failure: {error:#}"
    );
    fs::write(evidence.join("native-missing-first-beat.receipt"), format!(
        "scope=native ClusterHost and WAMN lifecycle helpers in test process\\nclosed_client_subscription_failed=true\\nheartbeat_interval_ms=100\\nsilence_budget_ms={}\\nobservation_ms={}\\nnever_beat=true\\ncleanup_polled_after_observer=true\\nnative_subscription_error_retrieved=true\\ncleanup_ms={}\\ncleanup_budget_ms={}\\nfull_process_unexpected_failure_proved=false\\n",
        silence_budget.as_millis(), observation_time.as_millis(), cleanup_time.as_millis(), REQUEST_BUDGET.as_millis(),
    ))?;
    Ok(())
}

'''
anchor='#[tokio::test(flavor = "multi_thread", worker_threads = 2)]\n#[ignore = "requires an owned real nats-server binary and a fresh external evidence directory"]'
assert source.count(anchor)==1 and 'async fn prove_missing_first_beat(' not in source
proposed=source.replace(anchor,helper+anchor)
proposed=proposed.replace('//! Black-box lifecycle proof for the rebuilt host and an owned real NATS server.','//! Lifecycle proof for the rebuilt host and an owned real NATS server.')
proposed=proposed.replace('//! directory. It starts no PostgreSQL, OCI registry, operator, or guest workload.','//! directory. It starts no PostgreSQL, OCI registry, operator, or guest workload.\n//! Missing-first-beat failure uses native objects and WAMN helpers in this test\n//! process; the signal and exporter cases exercise the rebuilt host child.')
call='    let result = async {\n        prove_host(&mut nats, &nats_binary, nats_address, &evidence, "TERM").await?;'
assert proposed.count(call)==1
proposed=proposed.replace(call,'    let result = async {\n        prove_missing_first_beat(nats_address, &evidence).await?;\n        prove_host(&mut nats, &nats_binary, nats_address, &evidence, "TERM").await?;')
(out/'native_lifecycle_live.rs').write_text(proposed)
patch=''.join(difflib.unified_diff(source.splitlines(keepends=True),proposed.splitlines(keepends=True),fromfile='a/'+path,tofile='b/'+path))
(out/'first-beat.patch').write_text(patch)
command='cargo test -p wamn-host --test native_lifecycle_live --locked --offline rebuilt_host_probes_signals_and_scheduler_recovery -- --ignored --exact --nocapture'
(out/'command.txt').write_text(command+'\n')
metadata={'schema':'wamn-native-first-beat-proof-preparation/v1','owner':'wamn-0h0g.2.7.3','status':'prepared_not_applied_not_compiled_not_executed','source_revision':subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip(),'source_path':path,'source_sha256':hashlib.sha256(source.encode()).hexdigest(),'proposed_sha256':hashlib.sha256(proposed.encode()).hexdigest(),'patch_sha256':hashlib.sha256(patch.encode()).hexdigest(),
 'existing_test_name':'rebuilt_host_probes_signals_and_scheduler_recovery','command':command,
 'arming':{'WAMN_HOST_LIVE_NATS_SERVER_BIN':'Absolute existing real NATS server binary, same existing fixture owner.','WAMN_HOST_LIVE_EVIDENCE_DIR':'Fresh absolute directory outside the source tree; parent exists and is outside the repository. The test creates it mode 0700.'},
 'new_receipt':'native-missing-first-beat.receipt','new_dependencies':[],
 'proof_steps':['Connect/flush a real owned NATS client, request drain, and await a failed valid subscription command. Check the public SubscribeErrorKind::Other so an invalid-subject refusal cannot satisfy the control.',
 'Supply the confirmed closed client to the actual native ClusterHostBuilder with a public 100ms heartbeat interval and native 3x (300ms) silence budget. Start without ingress, plugins or workloads.',
 'Wait on production WAMN watch_liveness without polling the opaque cleanup future. Require the first-beat error, silence None, and elapsed time inside the configured silence-plus-observer bound.',
 'After observer completion, drain ProbeState and use WAMN bounded_cleanup on the actual native cleanup future. Require a typed public async_nats SubscribeError and the native subscribe context, not a cleanup timeout or injected error.',
 'Drop the returned native HostApi handle. Retain a scope-labelled receipt and continue the existing full-process cases.'],
 'timeouts':{'connect_seconds':1,'connected_flush_seconds':1,'drain_request_seconds':1,'confirm_closed_subscription_seconds':1,'native_start_seconds':1,'watch_seconds':1.3,'native_cleanup_seconds':1,'existing_owned_nats_reap_seconds':5},
 'cleanup':'New helper creates no child process. It executes inside the existing captured-result async block, so all returned failures reach the existing NATS kill/reap tail. Real cleanup is polled after the observer even if the observer times out, then the HostApi handle is dropped before result assertions. No ingress/plugins/workloads are installed because the native subscribe error skips Host::stop.',
 'limits':['This proves an actual native detached subscribe-task failure observed by production WAMN lifecycle helpers in the test process. It does not prove the wamn-host executable unexpected-failure branch.',
 'The nonzero public 100ms heartbeat setting derives a 300ms silence bound. It does not measure the default 45-second silence interval.',
 'The existing WAMN process signal/exporter tests remain unchanged; this patch neither exposes hidden JoinHandles/errors nor polls cleanup as a passive monitor.',
 'Keep .2.7.3 open pending actual passing evidence.'],
 'static_validation':'Source/API inspection and local file/diff/JSON checks only. No Rust compiler, test, live tool, source edit or Beads mutation.'}
(out/'preparation.json').write_text(json.dumps(metadata,indent=2)+'\n')
assert (repo/path).read_text()==source
assert proposed.count('prove_missing_first_beat(nats_address, &evidence).await?;')==1
assert helper.index('let observed = timeout(')<helper.index('let cleaned = bounded_cleanup(')<helper.index('let error = observed')
assert '.beat(' not in helper and 'impl HostHandler' not in helper and 'spawn(' not in helper
result={'patch_sha256':metadata['patch_sha256'],'source_sha256':metadata['source_sha256'],'new_test_helper_lines':len(helper.splitlines()),'existing_test_owner_extended':True,'source_unchanged':True,'test_or_build_or_live_commands_run':0}
(out/'static-validation.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
