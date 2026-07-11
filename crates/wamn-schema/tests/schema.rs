//! Lifecycle + promotion tests over the canonical POC catalog (reused from
//! wamn-catalog's fixtures). Cover the state machine (legal transitions,
//! single-applied, stale-base rebase guard) and promotion (first CREATE,
//! additive, gated destructive, environment-aware), plus the storage-literal
//! drift guard tying State to deploy/catalog-schema.sql.

use std::path::{Path, PathBuf};

use wamn_catalog::{Catalog, Field, FieldType, Index};
use wamn_schema::{
    Action, Confirmation, Environment, LifecycleError, PromoteError, State, promote,
    promote_catalog, transition,
};

fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-catalog/tests/fixtures/poc-receiving.catalog.json")
}

/// The POC catalog at a given version number.
fn poc(version: u32) -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    let mut c = Catalog::from_json(&raw).expect("POC fixture parses");
    c.version = version;
    c
}

fn text_field(id: &str) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: FieldType::Text { max_len: None },
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

/// POC + an added nullable column on `materials` (an additive evolution).
fn poc_with_extra_column(version: u32) -> Catalog {
    let mut c = poc(version);
    let materials = c.entities.iter_mut().find(|e| e.id == "materials").unwrap();
    materials.fields.push(text_field("grade"));
    materials.indexes.push(Index {
        name: "materials_grade_idx".into(),
        fields: vec!["grade".into()],
        unique: false,
    });
    c
}

/// POC minus `suppliers.contact_email` (a destructive evolution).
fn poc_dropped_column(version: u32) -> Catalog {
    let mut c = poc(version);
    let suppliers = c.entities.iter_mut().find(|e| e.id == "suppliers").unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");
    c
}

// --- lifecycle -------------------------------------------------------------

#[test]
fn happy_path_draft_stage_apply() {
    let mut env = Environment::new("dev", "poc-material-receiving");
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
    let mut env = Environment::new("dev", "poc-material-receiving");
    env.add_draft(poc(1), None).unwrap();
    env.stage(1).unwrap();
    env.apply(1).unwrap();

    // v2 branches from the applied v1.
    env.add_draft(poc_with_extra_column(2), Some(1)).unwrap();
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
    let mut env = Environment::new("dev", "poc-material-receiving");
    env.add_draft(poc(1), None).unwrap();
    env.stage(1).unwrap();
    env.apply(1).unwrap();

    // Two candidates both branched from v1.
    env.add_draft(poc_with_extra_column(2), Some(1)).unwrap();
    env.add_draft(poc_with_extra_column(3), Some(1)).unwrap();
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
    let mut env = Environment::new("dev", "poc-material-receiving");
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
    let mut env = Environment::new("dev", "poc-material-receiving");
    env.add_draft(poc(1), None).unwrap();
    env.discard(1).expect("discard draft");
    assert!(env.record(1).is_none());
    assert!(env.applied().is_none());
}

#[test]
fn add_draft_rejects_mismatched_catalog_and_duplicates() {
    let mut env = Environment::new("dev", "poc-material-receiving");
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

// --- promotion -------------------------------------------------------------

#[test]
fn first_promotion_is_a_tenant_safe_create() {
    // Target empty -> a fresh CREATE, all additive.
    let plan = promote_catalog(&poc(1), None).expect("plan");
    assert!(plan.is_additive());
    assert!(!plan.requires_confirmation());
    assert_eq!(plan.target_version, None);

    let sql = plan.sql(Confirmation::None).expect("additive");
    assert!(sql.contains("CREATE TABLE \"receipts\""));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("current_setting('app.tenant', true)"));
}

#[test]
fn additive_promotion_needs_no_confirmation() {
    let plan = promote_catalog(&poc_with_extra_column(2), Some(&poc(1))).expect("plan");
    assert!(plan.is_additive(), "report: {}", plan.report());
    assert_eq!(plan.target_version, Some(1));
    let sql = plan.sql(Confirmation::None).expect("additive");
    assert!(sql.contains("ALTER TABLE \"materials\" ADD COLUMN \"grade\" text"));
}

#[test]
fn destructive_promotion_is_gated() {
    let plan = promote_catalog(&poc_dropped_column(2), Some(&poc(1))).expect("plan");
    assert!(plan.requires_confirmation());
    // Refused without confirmation…
    assert!(plan.sql(Confirmation::None).is_err());
    // …allowed with confirmation + backup, and marked.
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    assert!(sql.contains("BACKUP CHECKPOINT REQUIRED"));
    assert!(sql.contains("ALTER TABLE \"suppliers\" DROP COLUMN \"contact_email\""));
}

#[test]
fn promotion_warns_on_version_regression() {
    // Source version <= target's applied version is a non-fatal advisory.
    let plan = promote_catalog(&poc(1), Some(&poc(3))).expect("plan");
    assert!(plan.warnings.iter().any(|w| w.contains("not newer")));
}

#[test]
fn environment_aware_promote_dev_to_prod() {
    // dev has v2 applied; prod is empty -> first CREATE.
    let mut dev = Environment::new("dev", "poc-material-receiving");
    dev.add_draft(poc(1), None).unwrap();
    dev.stage(1).unwrap();
    dev.apply(1).unwrap();
    dev.add_draft(poc_with_extra_column(2), Some(1)).unwrap();
    dev.stage(2).unwrap();
    dev.apply(2).unwrap();

    let prod = Environment::new("prod", "poc-material-receiving");
    let plan = promote(&dev, &prod).expect("promote to empty prod");
    assert_eq!(plan.source_version, 2);
    assert_eq!(plan.target_version, None);
    assert!(plan.is_additive());
    assert!(
        plan.sql(Confirmation::None)
            .unwrap()
            .contains("CREATE TABLE \"materials\"")
    );
}

#[test]
fn promote_refuses_when_source_has_no_applied() {
    let dev = Environment::new("dev", "poc-material-receiving"); // empty
    let prod = Environment::new("prod", "poc-material-receiving");
    assert_eq!(promote(&dev, &prod), Err(PromoteError::NothingToPromote));
}

#[test]
fn promote_refuses_cross_catalog() {
    let mut dev = Environment::new("dev", "poc-material-receiving");
    dev.add_draft(poc(1), None).unwrap();
    dev.stage(1).unwrap();
    dev.apply(1).unwrap();
    let prod = Environment::new("prod", "other-catalog");
    assert!(matches!(
        promote(&dev, &prod),
        Err(PromoteError::CatalogIdMismatch { .. })
    ));
}

// --- storage drift guard ---------------------------------------------------

/// The `State` storage literals must match the `state` CHECK in
/// deploy/catalog-schema.sql (the crate is the source of truth for the values).
#[test]
fn state_literals_match_catalog_schema_sql() {
    let sql = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/catalog-schema.sql"),
    )
    .expect("read catalog-schema.sql");
    for s in State::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_sql())),
            "deploy/catalog-schema.sql is missing state literal {:?}",
            s.as_sql()
        );
    }
    // The single-applied invariant is a partial unique index.
    assert!(sql.contains("WHERE state = 'applied'"));
}

/// Sanity: the pure transition table agrees with the environment's behavior.
#[test]
fn transition_table_matches_environment() {
    assert!(transition(State::Draft, Action::Stage).is_some());
    assert!(transition(State::Applied, Action::Apply).is_none());
}
