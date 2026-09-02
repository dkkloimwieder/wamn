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

pub mod confinement;
pub mod intake;
