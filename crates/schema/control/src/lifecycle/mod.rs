//! wamn schema versioning & environments (3.4).
//!
//! A catalog does not go straight from edited to live. It moves through a
//! **lifecycle** — `draft → staged → applied` (with `superseded` for prior
//! applied versions) — and is **promoted** between **environments** (`dev`,
//! `canary`, `prod`). This crate owns that lifecycle; it
//! **composes** the shipped model crates rather than duplicating them:
//!
//! - [`wamn_schema_model`] (3.1) — the canonical model, its version
//!   [`wamn_schema_model::diff`], and the
//!   JSON import/export;
//! - [`wamn_control_registry`] (`wamn-q3n.1`) — the control-plane [`Triple`]
//!   `(org, project, env)` and the validated [`Env`] slug (the D18 generic env
//!   model), so an environment's identity uses the registry vocabulary.
//!
//! It provides:
//!
//! - **lifecycle** ([`State`], [`Action`], [`transition`]) — the pure state
//!   machine over catalog versions;
//! - **environments** ([`Environment`]) — a first-class deployment target that
//!   tracks one catalog's versions and enforces the two cross-version invariants:
//!   *single-applied* (one live version per environment) and the *stale-base
//!   rebase guard* (a staged candidate may be applied only while its base is
//!   still the current applied version);
//!
//! ## Scope
//!
//! This crate is the **lifecycle model**. It does **not** execute DDL or keep a
//! versioned migration history. Target reconciliation belongs to the
//! operations-only migration capability. The draft-editing designer UI
//! and the staging screen are 3.3; per-role RLS is 3.5. Version *storage* lives
//! in `deploy/sql/catalog-schema.sql` (the `state` / `environment` / `base_version`
//! columns + the single-applied partial-unique index) — this crate is the
//! in-memory model that storage persists.

mod environment;
mod state;

pub use environment::{Environment, LifecycleError, VersionRecord};
pub use state::{Action, Outcome, State, transition};

// Re-exported so callers construct environments without a direct dependency on
// wamn-control-registry.
pub use wamn_control_registry::{Env, Triple};
