//! # wamn-builder — the custom-node build service (5.5)
//!
//! Turns a tenant's node SOURCE into a signed, SBOM-carrying OCI artifact the
//! platform will run, in one isolated, credential-less sandbox (the 6.2 threat
//! class: no cluster creds, egress-restricted, resource/time-limited). The
//! pipeline stages, each a refuse-on-violation gate:
//!
//! 1. **dependency allowlist** ([`allowlist`], 5.5c) — `cargo metadata` over the
//!    node crate's resolved package set vs a pinned policy, BEFORE the build;
//! 2. **build** ([`build`], 5.5b) — `cargo build --release --target wasm32-wasip2`
//!    (a cdylib on the node SDK) or `jco componentize` (a JS/TS ES module);
//! 3. **import lint** (5.5a, [`wamn_host::egress_guard::screen_builder_component`])
//!    — the package allowlist + the interface tightening, over the built bytes;
//! 4. **sign + SBOM** ([`sign`] / [`sbom`], 5.5d) — an ed25519 detached signature
//!    over `sha256(wasm)` + a minimal CycloneDX SBOM;
//! 5. **OCI push** ([`registry`] + [`manifest_build`], 5.5e) — the wasm layer +
//!    the `wamn.node.manifest` / signature / SBOM annotations, pushed so the
//!    wash-runtime host can still pull it;
//! 6. **deployment emission** ([`deploy_emit`], 5.5f) — the serve-node runtime
//!    manifest with grants DERIVED from the imports (design-note 7).
//!
//! Spec: `docs/builder.md`.

pub mod allowlist;
pub mod build;
pub mod keygen;
pub mod manifest_build;
pub mod registry;
pub mod sbom;
pub mod sign;
