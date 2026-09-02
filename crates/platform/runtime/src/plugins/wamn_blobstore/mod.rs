//! The WAMN blobstore capability.
//!
//! WAMN is the single runtime owner of `wasmcloud:blobstore@0.1.0`. Upstream's
//! `wasi_blobstore` providers are prior-art source only and are never
//! registered — and, since the `wasi-blobstore` cargo feature is not enabled,
//! they are not even compiled, so a second runtime for this contract is a
//! structural impossibility rather than a convention (wamn-jpxo).
//!
//! This module holds the parts that decide, before any request exists, what a
//! guest is allowed to do:
//!
//! * [`confinement`] turns the descriptor's environment-owned bucket and
//!   prefix into refusals — an author may name an object, never a container.
//! * [`intake`] bounds a body and refuses to commit one it cannot prove
//!   complete, so a truncated stream cannot overwrite a good object under a
//!   deterministic key.
//!
//! # Two object-store semantics decided here, not inherited
//!
//! **`..` is refused as a SEGMENT, never as a substring.** A substring check
//! would also refuse the legitimate key `report..2026.zpl`, and a containment
//! rule that refuses valid names teaches authors to work around it.
//!
//! **The prefix separator is never doubled.** S3 treats `a//b` and `a/b` as
//! different keys, so a trailing slash on the environment-owned prefix would
//! put one logical object under two names — which breaks the deterministic-key
//! overwrite rule §2c depends on. Both prefix spellings resolve identically,
//! and a test says so.

pub mod bindings;
pub mod confinement;
pub mod intake;
