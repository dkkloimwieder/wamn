`apps/wamn_wms/` owns package `wamn_wms`, guest component `wms`, data crate `wamn-wms-data-access`, generated native UI crate `wamn-generated-wms-tui`, and test crate `wamn-wms-tests`.

The WMS example lives in `examples/wms_move.rs`.
The `wamn-client-terminal` package runs it through the `wms_move` Cargo example target.

From the repository root, run the local application tests:

```bash
cargo test --locked --offline -p wamn-wms-tests --lib
```

For disposable cluster tests, use the [WMS cluster test commands](../../docs/operations/build-and-test.md#wms-cluster-journey-wms-application-tests).
