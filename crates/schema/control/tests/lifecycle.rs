//! Lifecycle tests over the canonical POC catalog (reused from
//! wamn-schema-model's fixtures). Cover the state machine (legal transitions,
//! single-applied, stale-base rebase guard), plus the storage-literal drift
//! guards tying State to deploy/sql/catalog-schema.sql and asserting env is an
//! open slug.

use std::path::{Path, PathBuf};

use wamn_schema_control::lifecycle::{
    Action, Environment, LifecycleError, State, Triple, transition,
};
use wamn_schema_model::Catalog;

fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../model/tests/fixtures/poc-receiving.catalog.json")
}

/// An environment for the canonical POC application (`acme/receiving`) at `env`.
fn poc_env(env: &str) -> Environment {
    Environment::new(
        Triple::new("acme", "receiving", env),
        "poc-material-receiving",
    )
}

/// The POC catalog at a given version number.
fn poc(version: u32) -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    let mut c = Catalog::from_json(&raw).expect("POC fixture parses");
    c.version = version;
    c
}

fn poc_updated(version: u32) -> Catalog {
    let mut c = poc(version);
    let materials = c.entities.iter_mut().find(|e| e.id == "materials").unwrap();
    materials.description = Some("updated".into());
    c
}

// --- lifecycle -------------------------------------------------------------

#[test]
fn happy_path_draft_stage_apply() {
    let mut env = poc_env("dev");
    env.add_draft(poc(1), None).expect("first draft");
    assert_eq!(env.state_of(1), Some(State::Draft));

    env.stage(1).expect("stage");
    assert_eq!(env.state_of(1), Some(State::Staged));

    env.apply(1).expect("apply first version");
    assert_eq!(env.state_of(1), Some(State::Applied));
    assert_eq!(env.applied_version(), Some(1));
}

#[test]
fn applying_demotes_prior_applied_to_superseded() {
    let mut env = poc_env("dev");
    env.add_draft(poc(1), None).unwrap();
    env.stage(1).unwrap();
    env.apply(1).unwrap();

    // v2 branches from the applied v1.
    env.add_draft(poc_updated(2), Some(1)).unwrap();
    env.stage(2).unwrap();
    env.apply(2).unwrap();

    assert_eq!(env.state_of(1), Some(State::Superseded));
    assert_eq!(env.state_of(2), Some(State::Applied));
    assert_eq!(env.applied_version(), Some(2));
    // Single-applied: exactly one Applied version.
    assert_eq!(
        env.versions()
            .iter()
            .filter(|r| r.state == State::Applied)
            .count(),
        1
    );
}

#[test]
fn stale_base_guard_refuses_a_rebased_over_candidate() {
    let mut env = poc_env("dev");
    env.add_draft(poc(1), None).unwrap();
    env.stage(1).unwrap();
    env.apply(1).unwrap();

    // Two candidates both branched from v1.
    env.add_draft(poc_updated(2), Some(1)).unwrap();
    env.add_draft(poc_updated(3), Some(1)).unwrap();
    env.stage(2).unwrap();
    env.stage(3).unwrap();

    // Applying v2 succeeds and moves the applied pointer to v2.
    env.apply(2).unwrap();
    assert_eq!(env.applied_version(), Some(2));

    // v3's base (1) is now stale — the current applied is 2.
    let err = env.apply(3).unwrap_err();
    assert_eq!(
        err,
        LifecycleError::StaleBase {
            version: 3,
            base: Some(1),
            current_applied: Some(2),
        }
    );
    // v3 stays Staged; the schema is unchanged.
    assert_eq!(env.state_of(3), Some(State::Staged));
    assert_eq!(env.applied_version(), Some(2));
}

#[test]
fn cannot_apply_an_unstaged_draft() {
    let mut env = poc_env("dev");
    env.add_draft(poc(1), None).unwrap();
    let err = env.apply(1).unwrap_err();
    assert_eq!(
        err,
        LifecycleError::IllegalTransition {
            version: 1,
            from: State::Draft,
            action: Action::Apply,
        }
    );
}

#[test]
fn discard_removes_a_draft() {
    let mut env = poc_env("dev");
    env.add_draft(poc(1), None).unwrap();
    env.discard(1).expect("discard draft");
    assert!(env.record(1).is_none());
    assert!(env.applied().is_none());
}

#[test]
fn add_draft_rejects_mismatched_catalog_and_duplicates() {
    let mut env = poc_env("dev");
    // Fixture's catalog_id is not "other" — a mismatch.
    let mut wrong = poc(1);
    wrong.catalog_id = "other".into();
    assert!(matches!(
        env.add_draft(wrong, None),
        Err(LifecycleError::CatalogIdMismatch { .. })
    ));

    env.add_draft(poc(1), None).unwrap();
    assert_eq!(
        env.add_draft(poc(1), None),
        Err(LifecycleError::DuplicateVersion(1))
    );
}

// --- storage drift guard ---------------------------------------------------

/// The `State` storage literals must match the `state` CHECK in
/// deploy/sql/catalog-schema.sql (the crate is the source of truth for the values).
#[test]
fn state_literals_match_catalog_schema_sql() {
    let sql = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/sql/catalog-schema.sql"),
    )
    .expect("read catalog-schema.sql");
    for s in State::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_sql())),
            "deploy/sql/catalog-schema.sql is missing state literal {:?}",
            s.as_sql()
        );
    }
    // The single-applied invariant is a partial unique index.
    assert!(sql.contains("WHERE state = 'applied'"));
}

/// `environment` is an OPEN slug in the tenant catalog storage (D18): the column
/// exists and defaults to `dev`, but the closed `environment IN (...)` CHECK is
/// retired (env is data, resolved against the system registry's env_policies —
/// which a tenant catalog DB cannot FK).
#[test]
fn environment_is_an_open_slug_in_catalog_schema_sql() {
    let sql = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/sql/catalog-schema.sql"),
    )
    .expect("read catalog-schema.sql");
    assert!(sql.contains("environment    text NOT NULL DEFAULT 'dev'"));
    assert!(
        !sql.contains("environment IN ("),
        "the closed environment CHECK must be retired (D18 — env is an open slug)"
    );
}

/// Sanity: the pure transition table agrees with the environment's behavior.
#[test]
fn transition_table_matches_environment() {
    assert!(transition(State::Draft, Action::Stage).is_some());
    assert!(transition(State::Applied, Action::Apply).is_none());
}
