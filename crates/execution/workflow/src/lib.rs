//! The wiring layer: everything that walks or delivers a wiring.
//!
//! A wiring is a graph of operation nodes with edges. This crate will hold:
//!
//! - the walk, merged in from `wamn-router`,
//! - the driver glue that runs one node through `invoke_operation`,
//! - wiring delivery and its response,
//! - the queue and the enqueue path,
//! - Cron and registrations.
//!
//! The crate is empty until `wamn-xs9a.3` and `wamn-xs9a.4` move that code in.
//! The plan is `docs/plan/workflow-crate.md`.
//!
//! Every node calls `invoke_operation`. This crate owns no invocation path of
//! its own. A route never enters this crate.
//!
//! This crate sits above `wamn-execution-host` and below `services/host`. It
//! may depend on the host crate, and the host crate never depends on it.
//! `tests/dependency_boundary.rs` checks that `wamn-engine`, `wamn-runtime`,
//! and `wamn-execution-host` link neither this crate nor `wamn-router`.
