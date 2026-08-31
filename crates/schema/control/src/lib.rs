//! Pure package-migration, release, and project-schema reconciliation decisions.
//!
//! Application structure comes only from strict package manifests plus ordered
//! package-owned migration bytes. This crate has no legacy catalog model or DDL
//! compiler dependency and performs no filesystem, database, clock, or network
//! I/O. Effect shells execute each returned plan in one database transaction.

pub mod attestation;
pub mod connections;
pub mod exposure;
mod model;
mod package_migrations;
mod replica_identity;
mod run_plane;
pub mod sql;

pub use exposure::{
    Attachment, AttachmentKind, Cardinality, ExposureError, ExposureRelease, FlowExposure,
    HttpRoute, InputMapping, MappingSource, ResolvedAttachment, Source, SourceKind,
    resolve_exposure,
};
pub use model::{SqlStatement, Value};
pub use package_migrations::{
    AppliedPackage, ManagedModel, MigrationSource, PACKAGE_MANIFEST_DRIFT_REFUSAL,
    PACKAGE_MANIFEST_PATH, PACKAGE_MIGRATION_DRIFT_REFUSAL, PACKAGE_MIGRATION_DUPLICATE_REFUSAL,
    PACKAGE_MIGRATION_GAP_REFUSAL, PackageDirectory, PackageMigrationError,
    PackageMigrationErrorKind, PackageMigrationPlan, PendingMigration, RecordedMigration,
    plan_package_migrations,
};
pub use replica_identity::{
    ReplicaIdentity, ReplicaIdentityFlip, ReplicaIdentityPlan, UnreadableRegistrations,
    UnreadableRegistrationsKind, alter_replica_identity_sql, entities_requiring_full,
    reconcile_replica_identity, select_replica_identity_sql,
};
pub use run_plane::{
    BareSchemaName, EFFECT_WRITER_ROLE, EffectWriterRoleObservation, InvalidBareSchemaName,
    LEGACY_OUTBOX_TABLES, OUTBOX_TRIGGER_NAME, RowPolicyObservation, RowSecurityObservation,
    RunPlaneAction, RunPlaneActionKind, RunPlaneObservation, RunPlanePlan,
    ScenarioAuthorRoleObservation, catalog_schema_present_sql,
    count_retired_authored_ordering_rows_sql, count_stale_registration_keys_sql,
    ensure_scenario_author_role_sql, plan_run_plane, rewrite_schema,
    select_app_run_queue_authority_sql, select_app_scenario_author_membership_sql,
    select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_dispatch_reader_schema_privileges_sql,
    select_dispatch_reader_table_privileges_sql,
    select_effect_ledger_effective_column_privileges_sql,
    select_effect_ledger_effective_privileges_sql, select_effect_ledger_table_privileges_sql,
    select_effect_writer_role_sql, select_effect_writer_run_column_privileges_sql,
    select_effect_writer_run_table_privileges_sql, select_effect_writer_schema_privileges_sql,
    select_environment_policy_policies_sql, select_environment_policy_row_security_sql,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_run_capture_privileges_sql, select_run_plane_helper_functions_sql,
    select_scenario_author_role_sql, select_scenario_author_schema_usage_sql,
    select_schema_checks_sql, select_schema_columns_sql, select_schema_foreign_keys_sql,
    select_schema_indexes_sql, select_schema_triggers_sql, strip_retired_registration_keys_sql,
};

// Re-exported so a driver can name the registration type the reconciler folds
// without a direct dependency on wamn-event-reg.
pub use wamn_event_reg::EventRegistration;
