The first schema generation attempt fails during compilation at ce35909098bc6e2290470f6701a4583f17cb05f1.

The compiler reports E0433 at crates/platform/runtime/src/plugins/wamn_jetstream.rs:27 because wamn-control-provision exists only as a dev dependency. The command exits 101 in 19.740 seconds. The schema remains unchanged. Commit 090bbb67 moves the existing dependency into the runtime dependency list.
