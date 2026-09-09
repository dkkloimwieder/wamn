from pathlib import Path
import difflib, hashlib, json, subprocess
repo=Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
prepared=Path('/tmp/wamn-cutover-deadline-memory-prepared')
source_path='crates/platform/runtime/src/engine.rs'
source=(repo/source_path).read_text()
name='dropping_a_store_after_epoch_interruption_releases_its_memory'
test='''    #[tokio::test]
    async fn dropping_a_store_after_epoch_interruption_releases_its_memory() {
        let engine = build_engine_with_host_memory(&[], memory_budgets(2))
            .expect("build the production Count engine");
        let bytes = wat::parse_str(
            r#"(module
                (memory 2)
                (func (export "run") (loop br 0)))"#,
        )
        .expect("encode a memory-bearing guest that spins");
        let module = Module::new(engine.inner(), bytes).expect("compile the spinning guest");
        let ctx = SharedCtx::new(Ctx::builder("memory-proof", "memory-proof").build())
            .with_guest_memory(engine.guest_memory());
        let mut store = Store::new(engine.inner(), ctx);
        install_memory_limiter(&mut store);
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instantiate the spinning guest");
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("the fixture exports run");
        assert_eq!(engine.guest_memory().in_use(), 2 * PAGE as u64);

        let bound = Duration::from_secs(2);
        let started = std::time::Instant::now();
        store.set_epoch_deadline(1);
        let error = tokio::time::timeout(bound, run.call_async(&mut store, ()))
            .await
            .expect("the native epoch ticker must interrupt the guest within two seconds")
            .expect_err("the epoch deadline must interrupt the infinite guest");
        assert!(
            started.elapsed() < bound,
            "the epoch interruption exceeded {bound:?}"
        );
        assert!(
            matches!(
                error.downcast_ref::<wash_runtime::wasmtime::Trap>(),
                Some(wash_runtime::wasmtime::Trap::Interrupt)
            ),
            "the guest must stop with Trap::Interrupt, got {error:#}"
        );
        assert_eq!(engine.guest_memory().in_use(), 2 * PAGE as u64);
        drop(store);
        assert_eq!(engine.guest_memory().in_use(), 0);

        let (fresh, memory) = memory_store(&engine);
        assert_eq!(memory.size(&fresh), 1);
        assert_eq!(engine.guest_memory().in_use(), PAGE as u64);
        drop(fresh);
        assert_eq!(engine.guest_memory().in_use(), 0);
    }

'''
anchor='    #[tokio::test]\n    async fn dropping_a_store_after_a_guest_start_trap_releases_its_memory()'
assert source.count(anchor)==1 and name not in source
proposed=source.replace(anchor,test+anchor)
(prepared/'engine.rs').write_text(proposed)
patch=''.join(difflib.unified_diff(source.splitlines(keepends=True),proposed.splitlines(keepends=True),fromfile='a/'+source_path,tofile='b/'+source_path))
(prepared/'deadline-memory.patch').write_text(patch)
exact='engine::tests::'+name
command='cargo test -p wamn-runtime --lib --locked --offline '+exact+' -- --exact --nocapture --test-threads=1'
meta={
 'schema':'wamn-deadline-memory-preparation/v1','acceptance_owner':'wamn-0h0g.2.7.2','status':'proposed_not_applied_not_executed',
 'source_revision':'ab0467f415dfd6b31e70323ec6ca049d5d2a5298','source_path':source_path,
 'source_sha256':hashlib.sha256(source.encode()).hexdigest(),
 'proposed_source_sha256':hashlib.sha256(proposed.encode()).hexdigest(),
 'patch_sha256':hashlib.sha256(patch.encode()).hexdigest(),
 'test_name':exact,'scoped_command':command,
 'process_bounded_command':'timeout --signal=TERM --kill-after=5s 180s '+command,
 'arming':'No fixture, service, credentials, ignore flag, or new dependency. Root applies the one-test patch and runs the scoped test serially against the clean owner source.',
 'assertions':['A real two-page memory-bearing infinite-loop Wasm guest charges 128 KiB to the native engine budget.',
 'A one-tick native epoch deadline produces Trap::Interrupt within the two-second timeout.',
 'The interrupted but still-owned Store retains its 128 KiB charge; dropping that Store refunds usage to zero.',
 'The existing memory_store helper allocates a fresh page on the same engine, charges exactly 64 KiB, and refunds to zero when dropped.'],
 'reuse':['memory_budgets','memory_store','SharedCtx::with_guest_memory','install_memory_limiter','native Engine epoch ticker'],
 'scope_limits':['The outer process-bounded command also prevents a hung proof if a broken native ticker leaves Wasm polling without yielding; the internal timeout and elapsed assertion enforce the successful interruption bound.',
 'No WAMN manual ticker, new counter, production code, or dependency. This proves refund on owned Store drop after deadline interruption, not automatic release while a Store is still held.',
 'The existing epoch test with Store<()> proves interruption and engine reuse, but has no guest-memory charge/refund assertion.',
 'Keep wamn-0h0g.2.7.2 open pending actual execution and retained passing evidence.'],
 'static_review':'Construction, limiter, and charge assertions reuse current engine.rs tests; the timeout and Trap downcast reuse tests/system/src/deadlineproof.rs. No compile/test/live command was executed.'}
(prepared/'preparation.json').write_text(json.dumps(meta,indent=2)+'\n')
(prepared/'command.txt').write_text(command+'\n')
map_path=Path('/tmp/wamn-cutover-memory-cancellation-evidence-map.json')
data=json.loads(map_path.read_text())
gap={
 'requirement':'wamn-0h0g.2.7.2: memory released after deadline expiry',
 'status':'missing_direct_executed_evidence',
 'why_existing_receipts_do_not_close_it':'deadlineproof interrupts a Store<()> without linear memory or budget/refund assertions. Existing refund tests cover host-call cancellation and guest-start trap, not epoch interruption.',
 'smallest_proposed_test':exact,'prepared_patch':str(prepared/'deadline-memory.patch'),
 'command':command,'arming':'Apply the prepared unit test; no live fixture or new dependencies. Keep the bead open until root retains an actual passing test receipt.'}
data['remaining_required_arming']=[gap]
data['remaining_required_arming_note']='The seven charter areas retain the mapped scoped coverage, but additional bead acceptance wamn-0h0g.2.7.2 explicitly requires refund after deadline expiry. That direct case is proposed only and remains unproved.'
data['proposed_tests']=[{'name':exact,'status':'proposed_not_applied_not_executed','preparation':str(prepared/'preparation.json'),'command':command}]
map_path.write_text(json.dumps(data,indent=2)+'\n')
# File/diff inspection only. Do not invoke the proposed test or a compiler.
assert patch.count('@@ ')==1
assert sum(1 for x in patch.splitlines() if x.startswith('-') and not x.startswith('---'))==0
assert 'increment_epoch' not in test and 'spawn' not in test
assert json.loads(map_path.read_text())['remaining_required_arming'][0]['status']=='missing_direct_executed_evidence'
assert (repo/source_path).read_text()==source
validation={'scope':'offline file/diff inspection only; no compiler, formatter, tests or live commands',
 'one_added_test':True,'production_source_unchanged':True,'no_deleted_source_lines':True,
 'test_lines_added':len(test.splitlines()),'source_sha256':meta['source_sha256'],'patch_sha256':meta['patch_sha256'],
 'evidence_map_sha256':hashlib.sha256(map_path.read_bytes()).hexdigest(),'executed_test_count':0}
(prepared/'static-validation.json').write_text(json.dumps(validation,indent=2)+'\n')
# Supersede the previous map hash without claiming the new test was run.
prior_path=Path('/tmp/wamn-cutover-memory-cancellation-evidence-map-validation.json')
prior=json.loads(prior_path.read_text());prior['map_sha256']=validation['evidence_map_sha256'];prior['deadline_acceptance_gap_recorded']=True
prior['deadline_test_executed']=False;prior['supplemental_static_validation']=str(prepared/'static-validation.json')
prior_path.write_text(json.dumps(prior,indent=2)+'\n')
print(json.dumps(validation))
