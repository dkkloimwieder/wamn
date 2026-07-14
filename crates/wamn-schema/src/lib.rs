//! wamn schema versioning & environments (3.4).
//!
//! A catalog does not go straight from edited to live. It moves through a
//! **lifecycle** — `draft → staged → applied` (with `superseded` for prior
//! applied versions) — and is **promoted** between **environments** (`dev`,
//! `canary`, `prod`). This crate owns that lifecycle and promotion policy; it
//! **composes** the shipped model crates rather than duplicating them:
//!
//! - [`wamn_catalog`] (3.1) — the canonical model, its version [`diff`], and the
//!   JSON import/export that *is* the promotion format;
//! - [`wamn_ddl`] (3.2) — the DDL compiler and its additive/destructive
//!   confirmation gate, reused verbatim to compile a promotion's migration;
//! - [`wamn_registry`] (`wamn-q3n.1`) — the control-plane [`Triple`]
//!   `(org, project, env)` and the closed [`Env`] set, so an environment's
//!   identity and the "same application" promotion guard speak one vocabulary.
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
//! - **promotion** ([`promote`], [`promote_catalog`], [`PromotionPlan`]) — diff a
//!   source environment's applied catalog against a target's and compile the
//!   migration, carrying the 3.2 safety gate.
//!
//! ## Scope
//!
//! This crate is the **lifecycle + promotion model**. It does **not** execute
//! DDL, keep a versioned migration history, or roll back — that is the migration
//! engine (2.5), which wraps a [`PromotionPlan`]'s [`MigrationPlan`]. The real
//! backup / PITR mechanism is hosting (2.3 / 10.3); the draft-editing designer UI
//! and the staging screen are 3.3; per-role RLS is 3.5. Version *storage* lives
//! in `deploy/catalog-schema.sql` (the `state` / `environment` / `base_version`
//! columns + the single-applied partial-unique index) — this crate is the
//! in-memory model that storage persists.
//!
//! ```
//! use wamn_catalog::Catalog;
//! use wamn_schema::{Environment, Env, Triple, promote};
//!
//! # fn go(dev_applied: Catalog) -> Result<(), Box<dyn std::error::Error>> {
//! let app = |env| Triple::new("acme", "receiving", env);
//! let mut dev = Environment::new(app(Env::Dev), &dev_applied.catalog_id);
//! dev.add_draft(dev_applied, None)?; // first version
//! let v = dev.versions()[0].version();
//! dev.stage(v)?;
//! dev.apply(v)?; // now live in dev
//!
//! let prod = Environment::new(app(Env::Prod), dev.catalog_id());
//! let plan = promote(&dev, &prod)?;   // same app, prod empty -> a fresh CREATE
//! assert!(plan.is_additive());
//! # Ok(())
//! # }
//! ```

mod environment;
mod lifecycle;
mod promote;

pub use environment::{Environment, LifecycleError, VersionRecord};
pub use lifecycle::{Action, Outcome, State, transition};
pub use promote::{PromoteError, PromotionPlan, promote, promote_catalog};

// Re-exported for convenience so callers can drive the safety gate without a
// direct dependency on wamn-ddl.
pub use wamn_ddl::{Confirmation, MigrationPlan};

// Re-exported so callers construct environments and read the promotion
// vocabulary without a direct dependency on wamn-registry.
pub use wamn_registry::{Env, Triple};
