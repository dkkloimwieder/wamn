//! Management-owned authoring reads.
//!
//! One store remains: [`admission`], the project-database half, which is where
//! the gate verb now lives. Its control-database counterpart went with the
//! composition machinery (wamn-0h0g.8.5.5) — a store whose only writer and only
//! reader were both deleted by the same change is not a store.
//!
//! There is no draft store either. A draft is a CLIENT-SIDE file — a studio
//! buffer, a git working tree — and the wiring document's own content hash is
//! its identity, so nothing here persists a mutable authored document.

pub mod admission;
