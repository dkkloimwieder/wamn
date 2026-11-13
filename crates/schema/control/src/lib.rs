//! Pure package-migration, release, and project-schema reconciliation decisions.
//!
//! Application structure comes only from strict package manifests plus ordered
//! package-owned migration bytes. This crate has no legacy catalog model or DDL
//! compiler dependency and performs no filesystem, database, clock, or network
//! I/O. Effect shells execute each returned plan in one database transaction.

pub mod exposure;
pub mod migrations;
mod model;
mod package_migrations;
pub mod physical_schema;
mod replica_identity;
mod run_plane;
pub mod sql;

pub use exposure::{ExposureError, HttpRoute, canonical_http_route_template, normalize_http_route};
pub use model::{SqlStatement, Value};
pub use package_migrations::{
    AppliedPackage, CdcExcludedRelation, ManagedModel, MigrationOutcome, MigrationSource,
    PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL, PACKAGE_MANIFEST_PATH,
    PACKAGE_MIGRATION_DRIFT_REFUSAL, PACKAGE_MIGRATION_DUPLICATE_REFUSAL,
    PACKAGE_MIGRATION_GAP_REFUSAL, PackageDirectory, PackageMigrationError,
    PackageMigrationErrorType, PackageMigrationPlan, PendingMigration, RecordedMigration,
    plan_package_migrations, validate_package_schema_source,
};
pub use replica_identity::{
    ReplicaIdentity, ReplicaIdentityFlip, ReplicaIdentityPlan, alter_replica_identity_sql,
    reconcile_replica_identity, select_replica_identity_sql,
};
pub use run_plane::{
    BareSchemaName, InvalidBareSchemaName, RowPolicyObservation, RowSecurityObservation,
    RunPlaneAction, RunPlaneActionType, RunPlaneObservation, RunPlanePlan,
    catalog_schema_present_sql, plan_run_plane, rewrite_schema, select_app_run_queue_authority_sql,
    select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_environment_policy_policies_sql,
    select_environment_policy_row_security_sql, select_run_capture_privileges_sql,
    select_run_plane_helper_functions_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_indexes_sql, select_schema_triggers_sql,
};
