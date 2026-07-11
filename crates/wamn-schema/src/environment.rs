//! First-class environments (3.4).
//!
//! An **environment** (`dev`, `prod`, …) is a deployment target — in the
//! per-project-database model (2.2 / 2.3) it is a project's database. It holds
//! the lifecycle of **one catalog's** versions: which versions exist, their
//! [`State`], and which one is live. Two invariants live here, both of which
//! need cross-version context the pure [`crate::lifecycle`] table cannot see:
//!
//! - **single-applied** — at most one [`State::Applied`] version per environment;
//!   applying a Staged version demotes the previous Applied to
//!   [`State::Superseded`].
//! - **stale-base (rebase) guard** — a Staged version records the applied
//!   `base` version it was branched from; it may be applied only while that base
//!   is *still* the current Applied. If someone else applied a newer version in
//!   the meantime, the stale candidate is refused until it is rebased.
//!
//! Version numbers are **globally unique per catalog** (not per environment):
//! promotion mints a fresh version in the target environment, so `environment`
//! is an attribute of each version rather than part of its identity. This mirrors
//! `deploy/catalog-schema.sql`, where the single-applied invariant is a partial
//! unique index on `(tenant_id, catalog_id, environment) WHERE state = 'applied'`.

use wamn_catalog::Catalog;

use crate::lifecycle::{Action, Outcome, State, transition};

/// Why a lifecycle operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// No version with this number exists in the environment.
    UnknownVersion(u32),
    /// The catalog's `catalog_id` does not match the environment's.
    CatalogIdMismatch { expected: String, found: String },
    /// A version with this number already exists.
    DuplicateVersion(u32),
    /// The requested [`Action`] is illegal in the version's current [`State`].
    IllegalTransition {
        version: u32,
        from: State,
        action: Action,
    },
    /// The Staged version's `base` is no longer the current Applied version — it
    /// must be rebased onto the current schema before it can be applied.
    StaleBase {
        version: u32,
        base: Option<u32>,
        current_applied: Option<u32>,
    },
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::UnknownVersion(v) => write!(f, "no version {v} in this environment"),
            LifecycleError::CatalogIdMismatch { expected, found } => write!(
                f,
                "catalog id mismatch: environment tracks {expected:?}, got {found:?}"
            ),
            LifecycleError::DuplicateVersion(v) => write!(f, "version {v} already exists"),
            LifecycleError::IllegalTransition {
                version,
                from,
                action,
            } => write!(f, "cannot {action} version {version} in state {from}"),
            LifecycleError::StaleBase {
                version,
                base,
                current_applied,
            } => write!(
                f,
                "version {version} has a stale base ({base:?}); the current applied version is {current_applied:?} — rebase before applying"
            ),
        }
    }
}

impl std::error::Error for LifecycleError {}

/// One catalog version and its lifecycle position within an environment.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionRecord {
    /// The catalog content (owns its `catalog_id` and `version`).
    pub catalog: Catalog,
    /// The lifecycle state of this version.
    pub state: State,
    /// The applied version this one was branched from — `None` for the first
    /// version of a catalog. Checked by the stale-base guard at apply time.
    pub base: Option<u32>,
}

impl VersionRecord {
    /// The version number (`catalog.version`).
    pub fn version(&self) -> u32 {
        self.catalog.version
    }
}

/// An environment tracking the lifecycle of one catalog's versions.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    name: String,
    catalog_id: String,
    versions: Vec<VersionRecord>,
}

impl Environment {
    /// A fresh, empty environment named `name` tracking catalog `catalog_id`.
    pub fn new(name: impl Into<String>, catalog_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            catalog_id: catalog_id.into(),
            versions: Vec::new(),
        }
    }

    /// The environment name (`dev`, `prod`, …).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The catalog this environment tracks.
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    /// All version records, in insertion order.
    pub fn versions(&self) -> &[VersionRecord] {
        &self.versions
    }

    /// The record for `version`, if present.
    pub fn record(&self, version: u32) -> Option<&VersionRecord> {
        self.versions.iter().find(|r| r.version() == version)
    }

    /// The lifecycle state of `version`, if present.
    pub fn state_of(&self, version: u32) -> Option<State> {
        self.record(version).map(|r| r.state)
    }

    /// The currently-applied version record, if any.
    pub fn applied(&self) -> Option<&VersionRecord> {
        self.versions.iter().find(|r| r.state == State::Applied)
    }

    /// The currently-applied version number, if any.
    pub fn applied_version(&self) -> Option<u32> {
        self.applied().map(|r| r.version())
    }

    /// Add a new [`State::Draft`] version. `base` is the applied version it was
    /// branched from (`None` for a catalog's first version). Rejects a
    /// `catalog_id` mismatch or a duplicate version number.
    pub fn add_draft(&mut self, catalog: Catalog, base: Option<u32>) -> Result<(), LifecycleError> {
        if catalog.catalog_id != self.catalog_id {
            return Err(LifecycleError::CatalogIdMismatch {
                expected: self.catalog_id.clone(),
                found: catalog.catalog_id,
            });
        }
        if self.record(catalog.version).is_some() {
            return Err(LifecycleError::DuplicateVersion(catalog.version));
        }
        self.versions.push(VersionRecord {
            catalog,
            state: State::Draft,
            base,
        });
        Ok(())
    }

    /// Freeze a Draft version into a Staged candidate.
    pub fn stage(&mut self, version: u32) -> Result<(), LifecycleError> {
        self.simple_transition(version, Action::Stage)
    }

    /// Return a Staged candidate to Draft.
    pub fn unstage(&mut self, version: u32) -> Result<(), LifecycleError> {
        self.simple_transition(version, Action::Unstage)
    }

    /// Remove a Draft or Staged version.
    pub fn discard(&mut self, version: u32) -> Result<(), LifecycleError> {
        let from = self
            .state_of(version)
            .ok_or(LifecycleError::UnknownVersion(version))?;
        match transition(from, Action::Discard) {
            Some(Outcome::Removed) => {
                self.versions.retain(|r| r.version() != version);
                Ok(())
            }
            _ => Err(LifecycleError::IllegalTransition {
                version,
                from,
                action: Action::Discard,
            }),
        }
    }

    /// Make a Staged candidate the live schema. Enforces the stale-base guard
    /// (the candidate's `base` must equal the current applied version) and the
    /// single-applied invariant (the previous Applied is demoted to
    /// [`State::Superseded`]).
    pub fn apply(&mut self, version: u32) -> Result<(), LifecycleError> {
        let from = self
            .state_of(version)
            .ok_or(LifecycleError::UnknownVersion(version))?;
        // Legality first (only Staged -> Applied is legal).
        if transition(from, Action::Apply) != Some(Outcome::State(State::Applied)) {
            return Err(LifecycleError::IllegalTransition {
                version,
                from,
                action: Action::Apply,
            });
        }
        // Stale-base guard: the candidate must be built on the current Applied.
        let current = self.applied_version();
        let base = self.record(version).and_then(|r| r.base);
        if base != current {
            return Err(LifecycleError::StaleBase {
                version,
                base,
                current_applied: current,
            });
        }
        // Single-applied: demote the previous Applied, promote this one.
        for r in &mut self.versions {
            if r.state == State::Applied {
                r.state = State::Superseded;
            }
        }
        self.record_mut(version).expect("version present").state = State::Applied;
        Ok(())
    }

    fn record_mut(&mut self, version: u32) -> Option<&mut VersionRecord> {
        self.versions.iter_mut().find(|r| r.version() == version)
    }

    /// A transition with no cross-version consequences (stage / unstage).
    fn simple_transition(&mut self, version: u32, action: Action) -> Result<(), LifecycleError> {
        let from = self
            .state_of(version)
            .ok_or(LifecycleError::UnknownVersion(version))?;
        match transition(from, action) {
            Some(Outcome::State(to)) => {
                self.record_mut(version).expect("version present").state = to;
                Ok(())
            }
            _ => Err(LifecycleError::IllegalTransition {
                version,
                from,
                action,
            }),
        }
    }
}
