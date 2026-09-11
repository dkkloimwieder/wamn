The first scoped native run stops during compilation at 0811be17.

The compiler reports three E0277 errors in services/ctl/src/event_advisories.rs at lines 306, 336, and 614. Each native get_consumer call returns a boxed error that requires explicit conversion into anyhow::Error. No broker test executes. Commit 4449349e fixes those three conversions.

The command exits 101 in 25.409 seconds. Source bytes and file modes remain unchanged.
