//! The change feed of a list read: its cursor and one page of changes
//! (`docs/plan/wamn-sync.md`, "Query feed (host)").
//!
//! The cursor is opaque to the client: the URL-safe base64 of
//! `<model>:<transaction>` or `<model>:<transaction>:<id>`. `transaction` is
//! the snapshot xmin the read started from. A cut page below xmin also carries
//! the transaction and the id of its last row. A page with visible rows at or
//! above xmin completes the pass and keeps its next cursor at xmin.

pub mod id;
