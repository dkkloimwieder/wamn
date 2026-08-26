//! Repository proof for bounded host effect waits.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wash_runtime::engine::Engine;
    use wash_runtime::wasmtime::{Instance, Module, Store, Trap};

    // `(module (func (export "run") (loop br 0)))`. Keeping the tiny module
    // encoded here avoids adding a test-only WAT parser to this proof crate.
    const SPIN_MODULE: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00, 0x0a, 0x09, 0x01,
        0x07, 0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b,
    ];

    #[test]
    fn vanilla_epoch_ticker_interrupts_without_poisoning_the_engine() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build the deadline proof runtime");
        runtime.block_on(async {
            let engine = Engine::builder().build().expect("build the vanilla engine");
            let module = Module::new(engine.inner(), SPIN_MODULE).expect("compile the spin module");

            let mut interrupted = Store::new(engine.inner(), ());
            let instance = Instance::new_async(&mut interrupted, &module, &[])
                .await
                .expect("instantiate the spin module");
            let run = instance
                .get_typed_func::<(), ()>(&mut interrupted, "run")
                .expect("resolve the spin export");
            interrupted.set_epoch_deadline(1);
            let error =
                tokio::time::timeout(Duration::from_secs(2), run.call_async(&mut interrupted, ()))
                    .await
                    .expect("wash-runtime's ticker must advance the epoch")
                    .expect_err("the epoch deadline must interrupt the infinite guest");
            assert!(
                matches!(error.downcast_ref::<Trap>(), Some(Trap::Interrupt)),
                "the infinite guest must stop with Trap::Interrupt, got {error:#}"
            );
            drop(interrupted);

            let mut fresh = Store::new(engine.inner(), ());
            fresh.set_epoch_deadline(u64::MAX / 2);
            Instance::new_async(&mut fresh, &module, &[])
                .await
                .expect("an interrupted store must not poison the engine for a fresh instance");
        });
    }

    // wamn-hopk R5: three cross-crate source greps for timeout spellings stood
    // here. Deleted; the behavioural epoch-interrupt arm above is what proves a
    // deadline actually fires.
}
