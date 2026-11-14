//! Pure SQL text builders for the T1 control-plane registry (wamn-q3n.6).
//!
//! Registry SQL lives with the registry model (SR2: the single source, like
//! `wamn-run-state` owns the `runs` SQL), drift-guarded against the storage DDL
//! in `deploy/sql/system-schema.sql`. Values travel as `$n` params; the driver (the
//! `provision-org` subcommand) holds the `wamn_system` connection and executes
//! the statement as the registry owner.

/// Upsert an org's placement row into `registry.orgs` (idempotent + additive —
/// re-running `provision-org` refreshes placement, never dropping). Params: `$1`
/// id, `$2` placement_type (`pooled` / `dedicated`), `$3` pool_cluster (nullable
/// `text` — the shared pool for a pooled org, `NULL` for a dedicated org whose
/// clusters are derived, D18 the provisioning placement rule).
///
/// The placement-type CHECK and the `pooled ⟺ pool_cluster` structural CHECK are
/// enforced by the schema, not re-validated here — a bad row is rejected by the DB.
pub fn upsert_org_sql() -> &'static str {
    "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
     VALUES ($1, $2, $3) \
     ON CONFLICT (id) DO UPDATE SET \
       placement_type = EXCLUDED.placement_type, \
       pool_cluster = EXCLUDED.pool_cluster"
}

/// Select an org's placement (`placement_type`, `pool_cluster`) by id, so
/// `provision-project-env` (wamn-q3n.7) can derive the target cluster per-env via
/// the provisioning placement rule (placement + the env policy) — without
/// requiring a project database to exist. Param: `$1` org id.
pub fn select_org_placement_sql() -> &'static str {
    "SELECT placement_type, pool_cluster FROM registry.orgs WHERE id = $1"
}

// --- env policies (wamn-8df.3; org-targeted by wamn-8df.4) --------------------
//
// The per-org [`EnvPolicy`](crate::EnvPolicy) rows (D18): sizing / HA / backup /
// recovery-domain per (org, env slug). `recovery_domain` is `jsonb`; the reads
// cast it to `text` so the driver serde-parses it back into `RecoveryDomain`.
// Stamped from a [`Template`](crate::Template) at provision-org time (no
// platform-global seed); columns drift-guarded against the storage DDL. The
// org's full set is what `provision-org` sizes clusters from and what
// `provision-project-env` derives the cluster owner from.

/// The `registry.env_policies` policy-value column list, in the order both reads
/// return and a row-mapper reads by index (`org` is the caller's key, not
/// returned). `recovery_domain` is cast to `text` for serde.
const ENV_POLICY_COLUMNS: &str = "name, recovery_domain::text, promotion_rank, instances, \
     storage, cpu, memory, image, backup_cadence, wal_retention, hibernation";

/// Select one org's whole env-policy set, ordered by `promotion_rank` (so
/// `provision-org` sees `dev` before `prod`). Param: `$1` org id. Columns:
/// `ENV_POLICY_COLUMNS`.
pub fn select_env_policies_sql() -> String {
    format!(
        "SELECT {ENV_POLICY_COLUMNS} FROM registry.env_policies \
         WHERE org = $1 ORDER BY promotion_rank"
    )
}

/// Select one env policy from an org's set — `provision-project-env` reads it to
/// derive the project-env's cluster owner (and confirm the env resolves to one of
/// the org's policies). Params: `$1` org id, `$2` policy name (the env slug).
/// Columns: `ENV_POLICY_COLUMNS`.
pub fn select_env_policy_sql() -> String {
    format!(
        "SELECT {ENV_POLICY_COLUMNS} FROM registry.env_policies \
         WHERE org = $1 AND name = $2"
    )
}

/// Stamp one template policy row into an org's set — **insert-if-absent**
/// (`ON CONFLICT (org, name) DO NOTHING`), the wamn-8df.4 instantiate-and-own
/// semantics: re-running `provision-org` keeps an org's per-env customizations
/// and only adds envs the org is missing; it never clobbers a customized row
/// back to template values. Params: `$1` org, `$2` name, `$3` recovery_domain
/// (JSON text, cast `::text::jsonb`), `$4` promotion_rank, `$5` instances, `$6`
/// storage, `$7` cpu, `$8` memory, `$9` image, `$10` backup_cadence, `$11`
/// wal_retention, `$12` hibernation.
pub fn stamp_env_policy_sql() -> &'static str {
    "INSERT INTO registry.env_policies \
       (org, name, recovery_domain, promotion_rank, instances, \
        storage, cpu, memory, image, backup_cadence, wal_retention, hibernation) \
     VALUES ($1, $2, $3::text::jsonb, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
     ON CONFLICT (org, name) DO NOTHING"
}

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
pub fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// The `registry.environment_projects` column list, in the order both reads return and a
/// row-mapper reads by index. `org` is the caller's key in both reads, so it is
/// not returned; the remaining columns are exactly what a
/// project deployment needs (the triple's `org` comes from the
/// parameter). `instance_suffix` is here because nothing else can supply it — it
/// is the one part of a derived physical name that the triple does not carry
/// (wamn-0h0g.15.89).
const PROJECT_ENV_COLUMNS: &str = "env, project, secret_name, secret_namespace, instance_suffix";

/// Read ONE provisioned project-env by its identity triple — the read a consumer
/// that derives physical names needs, since a triple alone cannot yield the
/// environment's `instance_suffix` and only the provisioner sees it come back
/// from [`upsert_project_env_with_policy_sql`]. Params: `$1` org, `$2` project, `$3` env.
/// Columns: `PROJECT_ENV_COLUMNS`.
pub fn select_project_env_sql() -> String {
    format!(
        "SELECT {PROJECT_ENV_COLUMNS} FROM registry.environment_projects \
         WHERE org = $1 AND project = $2 AND env = $3"
    )
}

/// Record the environment, then its project database. Parameters follow the caller's
/// org, project, environment, Secret, namespace, suffix, disposable, policy, target values.
pub fn upsert_project_env_with_policy_sql() -> &'static str {
    "WITH parent AS (INSERT INTO registry.environments (org,env,disposable,policy_name,target_id) \
       VALUES ($1,$3,COALESCE($7,false),$8,$9::text::uuid) \
       ON CONFLICT (org,env) DO UPDATE SET disposable=COALESCE($7,registry.environments.disposable), \
         target_id=EXCLUDED.target_id RETURNING org,env,disposable), \
     deployment AS (INSERT INTO registry.environment_projects \
       (org,env,project,secret_name,secret_namespace,instance_suffix) \
       SELECT org,env,$2,$4,$5,$6 FROM parent \
       ON CONFLICT (org,env,project) DO UPDATE SET secret_name=EXCLUDED.secret_name, \
         secret_namespace=EXCLUDED.secret_namespace RETURNING instance_suffix) \
     SELECT deployment.instance_suffix,parent.disposable FROM deployment CROSS JOIN parent"
}

// --- event readers (wamn-l5i9.9, D19 v3) ------------------------------------
//
// The `registry.event_readers` row an `enable-cdc-project-env` overlay records:
// which publication + failover slot a project-env's CDC reader streams from,
// which JetStream stream it publishes into, and the REFERENCE to its
// replication-credential Secret (invariant 2 — never the material). Keyed by
// the (org, project, env) triple, FK'd to the provisioned project-env (the
// `provisioning.dumps` precedent), so a de-provisioned env drops its
// registration. The reader service (l5i9.10) reads its row to learn what to
// stream. Columns drift-guarded against the storage DDL.

/// Upsert a project-env's CDC reader registration (idempotent + refreshing —
/// re-enabling refreshes the names/stream/Secret reference and re-arms
/// `enabled`). Params: `$1` org, `$2` project, `$3` env, `$4` publication, `$5`
/// slot, `$6` stream, `$7` replication_secret_name, `$8`
/// replication_secret_namespace (nullable), `$9` enabled, `$10` schema, the
/// application schema that the publication covers.
pub fn upsert_event_reader_sql() -> &'static str {
    "INSERT INTO registry.event_readers \
       (org, project, env, publication, slot, stream, \
        replication_secret_name, replication_secret_namespace, enabled, schema) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       publication = EXCLUDED.publication, \
       schema = EXCLUDED.schema, \
       slot = EXCLUDED.slot, \
       stream = EXCLUDED.stream, \
       replication_secret_name = EXCLUDED.replication_secret_name, \
       replication_secret_namespace = EXCLUDED.replication_secret_namespace, \
       enabled = EXCLUDED.enabled, \
       updated_at = now()"
}

/// Read the newest capture gap of one CDC reader registration (`wamn-59z6`).
/// Params: `$1` org, `$2` project, `$3` env. Columns: `slot` (the lost slot)
/// and whether `resync_at` is set. No row: the registration never lost a slot.
pub fn select_capture_gap_sql() -> &'static str {
    "SELECT slot, resync_at IS NOT NULL \
     FROM registry.capture_gap \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY created_at DESC LIMIT 1"
}

/// Read what `recover-capture-gap` needs of one CDC reader registration
/// (`wamn-59z6`). Params: `$1` org, `$2` project, `$3` env. Columns: `slot`,
/// `stream`, and `created_at`, the start of a gap on a stream with no CDC event.
pub fn select_event_reader_origin_sql() -> &'static str {
    "SELECT slot, stream, created_at \
     FROM registry.event_readers \
     WHERE org = $1 AND project = $2 AND env = $3"
}

/// Record one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the lost slot, `$5` start LSN as text (nullable), `$6` start time,
/// `$7` reason, `$8` end LSN as text. Returns `created_at`.
pub fn insert_capture_gap_sql() -> &'static str {
    "INSERT INTO registry.capture_gap \
       (org, project, env, slot, start_lsn, start_at, reason, end_lsn) \
     VALUES ($1, $2, $3, $4, $5::text::pg_lsn, $6, $7, $8::text::pg_lsn) \
     RETURNING created_at"
}

/// Lock the newest capture gap of one registration (`wamn-59z6`). Params: `$1`
/// org, `$2` project, `$3` env. Columns: `created_at`, `slot`, and `resync_at`.
pub fn lock_newest_capture_gap_sql() -> &'static str {
    "SELECT created_at, slot, resync_at \
     FROM registry.capture_gap \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY created_at DESC LIMIT 1 FOR UPDATE"
}

/// Close one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the row's `created_at`. Returns `resync_at`.
pub fn close_capture_gap_sql() -> &'static str {
    "UPDATE registry.capture_gap SET resync_at = now() \
     WHERE org = $1 AND project = $2 AND env = $3 AND created_at = $4 \
     RETURNING resync_at"
}

/// Delete every connection of one environment, with no reference refusal, and
/// disable their service principals. Generations, credential rows and the
/// token row cascade. Params: `$1` org, `$2` project, `$3` env.
/// Returns the number of removed credentials.
pub fn delete_environment_connections_sql() -> &'static str {
    "WITH removed AS ( \
       DELETE FROM registry.connection WHERE org = $1 AND project = $2 AND env = $3 \
       RETURNING id, service_principal_id), \
     disabled AS ( \
       UPDATE identity.principals SET status = 'disabled', disabled_at = transaction_timestamp() \
       WHERE id IN (SELECT service_principal_id FROM removed) AND status <> 'disabled') \
     SELECT count(*) FROM identity.credential k \
     JOIN removed r ON r.id = k.connection_id"
}
/// Environment exclusion key shared by lifecycle operations and CDC publication.
/// Acquire this key before an application key or a database mutation lock.
pub const ENVIRONMENT_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Independent applications and SQL Jobs exclude shared database replacement.
pub const ENVIRONMENT_SHARED_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock_shared(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Lifecycle state belongs to the environment.
pub const ENVIRONMENT_DISPOSABLE: &str =
    "SELECT disposable FROM registry.environments WHERE org=$1 AND env=$2";

/// crates/execution/host/src/credential_cleanup.rs::collect registry statement.
pub const EXECUTION_HOST_SRC_CREDENTIAL_CLEANUP_COLLECT: &str =
    "SELECT store,ref,version FROM registry.retired_connection_credential($1)";

/// crates/execution/host/src/file_route.rs::generations registry statement.
pub const EXECUTION_HOST_SRC_FILE_ROUTE_GENERATIONS: &str = "SELECT connection_id::text, connection_name, generation, parameters FROM registry.read_file_connection($1,$2,$3)";

/// crates/execution/host/src/control_route/integration.rs::integration_administration_in registry statement.
pub const EXECUTION_HOST_SRC_CONTROL_ROUTE_INTEGRATION_INTEGRATION_ADMINISTRATION_IN: &str = "SELECT project,env FROM registry.environment_projects WHERE org=$1 AND project=$2 \
             AND ($3::text IS NULL OR env=$3) ORDER BY env";

/// crates/execution/host/src/control_route/operation_job.rs::operation_job registry statement.
pub const EXECUTION_HOST_SRC_CONTROL_ROUTE_OPERATION_JOB_OPERATION_JOB: &str =
    "SELECT 1 FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3";

/// crates/platform/runtime/src/oauth_token.rs::lock registry statement.
pub const PLATFORM_RUNTIME_SRC_OAUTH_TOKEN_LOCK: &str =
    "SELECT registry.lock_oauth_connection($1,$2,$3,$4)";

/// crates/platform/runtime/src/connection_credentials.rs::integration_version registry statement.
pub const PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_INTEGRATION_VERSION: &str =
    "SELECT version, digest FROM registry.read_integration_version($1,$2)";

/// crates/platform/runtime/src/connection_credentials.rs::env_connections registry statement.
pub const PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_ENV_CONNECTIONS: &str =
    "SELECT name, id FROM registry.read_env_connections($1,$2,$3)";

/// crates/platform/runtime/src/connection_credentials.rs::credential_secret registry statement.
pub const PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CREDENTIAL_SECRET: &str =
    "SELECT store, ref, version FROM registry.read_connection_secret($1,$2,$3,$4)";

/// crates/platform/runtime/src/connection_credentials.rs::connection_generation registry statement.
pub const PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CONNECTION_GENERATION: &str = "SELECT name, integration_type, runtime_type, auth_type, external_system, parameters FROM registry.read_connection_generation($1,$2,$3,$4,$5)";

/// crates/platform/runtime/src/connection_credentials.rs::connection_fact registry statement.
pub const PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CONNECTION_FACT: &str = "SELECT active_generation, active, credential_id, exclusive, target_id, resource_key, target_type, service_principal_id FROM registry.read_connection_fact($1,$2,$3,$4)";

/// crates/identity/platform/src/application.rs::project_authority registry statement.
pub const IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY: &str =
    "SELECT platform_domain FROM registry.meta";

/// crates/identity/platform/src/control.rs::CONTROL_ORGS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_CONTROL_CONTROL_ORGS_SQL: &str = "SELECT o.id FROM registry.orgs o \
    JOIN identity.principals p ON p.id = $1::text::uuid \
    WHERE p.type = 'user' AND p.status = 'active' \
    AND EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=o.id AND m.status='active') \
    AND (EXISTS (SELECT 1 FROM identity.project_roles r \
    WHERE r.principal_id = p.id AND r.org = o.id AND r.role = $2) \
    OR EXISTS (SELECT 1 FROM identity.org_roles r \
    WHERE r.principal_id = p.id AND r.org = o.id AND r.role = $3) \
    OR EXISTS (SELECT 1 FROM identity.environment_roles r WHERE r.principal_id=p.id AND r.org=o.id)) \
    AND ($4::text IS NULL OR o.id = $4) AND ($5::text IS NULL OR o.id > $5) \
    ORDER BY o.id LIMIT $6";

/// crates/identity/platform/src/control.rs::ORG_PROJECTS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_CONTROL_ORG_PROJECTS_SQL: &str = "SELECT p.id, CASE WHEN EXISTS (SELECT 1 FROM registry.environment_projects e JOIN registry.environments v USING (org,env) \
    WHERE e.org = p.org AND e.project = p.id AND v.status = 'active') \
    THEN 'active' ELSE 'inactive' END FROM registry.projects p WHERE p.org = $1 \
    AND ($2::text IS NULL OR p.id > $2) ORDER BY p.id LIMIT $3";

/// crates/identity/platform/src/control.rs::CONTROL_PROJECTS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_CONTROL_CONTROL_PROJECTS_SQL: &str = "SELECT r.id FROM registry.projects r \
    JOIN identity.principals p ON p.id=$1::text::uuid \
    WHERE r.org=$2 AND p.type='user' AND p.status='active' \
    AND EXISTS (SELECT FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active') \
    AND (EXISTS (SELECT FROM identity.project_roles a WHERE a.principal_id=p.id AND a.org=$2 AND a.project=r.id AND a.role=$3) \
      OR EXISTS (SELECT FROM identity.org_roles a WHERE a.principal_id=p.id AND a.org=$2) \
      OR EXISTS (SELECT FROM identity.environment_roles a JOIN registry.environment_projects e USING (org,env) WHERE a.principal_id=p.id AND a.org=$2 AND e.project=r.id)) \
    AND ($4::text IS NULL OR r.id>$4) ORDER BY r.id LIMIT $5";

/// crates/identity/platform/src/connection_projection.rs::document registry statement.
pub const IDENTITY_PLATFORM_SRC_CONNECTION_PROJECTION_DOCUMENT: &str =
    "SELECT registry.connection_projection($1,$2,$3)";

/// crates/identity/platform/src/org.rs::PROJECT_MEMBERS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_PROJECT_MEMBERS_SQL: &str = "SELECT p.id::text,p.email,p.display_name, \
    EXISTS (SELECT FROM identity.org_roles r WHERE r.principal_id=p.id AND r.org=$1 AND r.role=$4), \
    identity.is_project_admin(p.id,$1,$8,$2) AND $3='project-admin', \
    ARRAY(SELECT e.env FROM registry.environment_projects e WHERE e.org=$1 AND e.project=$2 \
      AND ($8::text IS NULL OR e.env=$8) AND identity.has_project_access(p.id,$1,e.env,$2) ORDER BY e.env) \
    FROM identity.principals p WHERE p.type='user' \
    AND EXISTS (SELECT FROM registry.environment_projects e WHERE e.org=$1 AND e.project=$2 \
      AND ($8::text IS NULL OR e.env=$8) AND identity.has_project_access(p.id,$1,e.env,$2)) \
    AND ($5::text IS NULL OR (p.email,p.id)>($5,$6::text::uuid)) ORDER BY p.email,p.id LIMIT $7";

/// crates/identity/platform/src/org.rs::PROJECT_ENVS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_PROJECT_ENVS_SQL: &str = "SELECT env, status FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org = $1 AND project = $2 \
    AND ($3::text IS NULL OR env > $3) ORDER BY env LIMIT $4";

/// crates/identity/platform/src/org.rs::ENVIRONMENT_STATUS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_ENVIRONMENT_STATUS_SQL: &str = "UPDATE registry.environments SET status = $4 \
    WHERE org = $1 AND env = ANY($3) AND EXISTS (SELECT FROM registry.environment_projects p WHERE p.org=$1 AND p.env=registry.environments.env AND p.project=$2)";

/// crates/identity/platform/src/org.rs::ORG_ENVIRONMENTS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_ORG_ENVIRONMENTS_SQL: &str = "SELECT project, env FROM registry.environment_projects \
    WHERE org = $1 AND ($2::text IS NULL OR project = $2) ORDER BY project, env";

/// crates/identity/platform/src/org.rs::CREATE_PROJECT_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_CREATE_PROJECT_SQL: &str =
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) ON CONFLICT (org, id) DO NOTHING";

/// crates/identity/platform/src/org.rs::ENV_IN_PROJECT_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_ENV_IN_PROJECT_SQL: &str = "SELECT EXISTS (SELECT 1 FROM registry.environment_projects \
    WHERE org = $1 AND project = $2 AND env = $3)";

/// crates/identity/platform/src/org.rs::PROJECT_IN_ORG_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_PROJECT_IN_ORG_SQL: &str =
    "SELECT EXISTS (SELECT 1 FROM registry.projects WHERE org = $1 AND id = $2)";

/// crates/identity/platform/src/connections/administration.rs::configure registry statement.
pub const IDENTITY_PLATFORM_SRC_CONNECTIONS_ADMINISTRATION_CONFIGURE: &str =
    "SELECT registry.configure_connection($1,$2,$3::text::jsonb)::text";

/// crates/identity/platform/src/connections/administration.rs::configure registry statement.
pub const IDENTITY_PLATFORM_SRC_CONNECTIONS_ADMINISTRATION_CONFIGURE_2: &str =
    "SELECT registry.configure_integration_type($1,$2,$3::text::jsonb)::text";

/// crates/identity/platform/src/connections/administration.rs::configure registry statement.
pub const IDENTITY_PLATFORM_SRC_CONNECTIONS_ADMINISTRATION_CONFIGURE_3: &str =
    "SELECT registry.configure_integration($1,$2,$3::text::jsonb)::text";

/// crates/identity/platform/src/connections/administration.rs::configure registry statement.
pub const IDENTITY_PLATFORM_SRC_CONNECTIONS_ADMINISTRATION_CONFIGURE_4: &str =
    "SELECT registry.configure_external_system($1,$2,$3::text::jsonb)::text";

/// crates/control/dev/src/pat_secrets_tests.rs::auth_drops_mint_pat_records_the_committed_project_role_drop registry statement.
pub const CONTROL_DEV_SRC_PAT_SECRETS_TESTS_AUTH_DROPS_MINT_PAT_RECORDS_THE_COMMITTED_PROJECT_ROLE_DROP: &str = "INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')";

/// crates/control/dev/src/dev/target_database.rs::recorded_disposable registry statement.
pub const CONTROL_DEV_SRC_DEV_TARGET_DATABASE_RECORDED_DISPOSABLE: &str = "SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3";

/// crates/control/provision/src/sql.rs::control_surface_grants_sql registry statement.
pub const CONTROL_PROVISION_SRC_SQL_CONTROL_SURFACE_GRANTS_SQL: &str = " GRANT EXECUTE ON FUNCTION registry.control_org(), identity.is_project_admin(uuid,text,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_control;";

/// crates/control/provision/src/sql.rs::control_surface_grants_sql registry statement.
pub fn control_provision_src_sql_control_surface_grants_sql_2(
    role: &str,
    signature: &str,
) -> String {
    format!(
        " DO $connection_acl$ BEGIN IF to_regprocedure('registry.{signature}') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.{signature} TO {role}; END IF; END $connection_acl$;"
    )
}

/// crates/control/provision/src/sql.rs::grant_connection_reader_surface_sql registry statement.
pub fn control_provision_src_sql_grant_connection_reader_surface_sql(role: &str) -> String {
    format!(
        " REVOKE ALL PRIVILEGES ON ALL FUNCTIONS IN SCHEMA registry, identity FROM {role}; GRANT EXECUTE ON FUNCTION registry.lock_oauth_connection(text,text,text,uuid), registry.read_file_connection(text,text,text,uuid), registry.read_connection_fact(text,text,text,uuid), registry.read_connection_generation(text,text,text,uuid,bigint), registry.read_connection_secret(text,text,text,uuid), registry.read_env_connections(text,text,text), registry.read_integration_version(text,integer) TO {role};"
    )
}

/// crates/control/environment/src/provision_system.rs::set_platform_domain registry statement.
pub const CONTROL_ENVIRONMENT_SRC_PROVISION_SYSTEM_SET_PLATFORM_DOMAIN: &str =
    "UPDATE registry.meta SET platform_domain = $1 WHERE platform_domain IS DISTINCT FROM $1";

/// crates/control/environment/src/invite.rs::invite registry statement.
pub const CONTROL_ENVIRONMENT_SRC_INVITE_INVITE: &str =
    "SELECT id FROM registry.orgs WHERE id = $1 FOR UPDATE";

/// crates/control/environment/src/provision_project_env/registry.rs::do_record_project_env registry statement.
pub const CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_DO_RECORD_PROJECT_ENV: &str =
    "SELECT id::text FROM registry.target WHERE name = $1";

/// crates/control/environment/src/provision_project_env/registry.rs::read_project_env_policy registry statement.
pub const CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_READ_PROJECT_ENV_POLICY: &str = "SELECT policy_name FROM registry.environment_projects JOIN registry.environments USING (org,env) \
                  WHERE org = $1 AND project = $2 AND env = $3";

/// crates/control/environment/src/environment/analyse.rs::replica_refusals registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_REPLICA_REFUSALS: &str =
    "SELECT type = 'cloud' FROM registry.target WHERE name = $1";

/// crates/control/environment/src/environment/analyse.rs::read_row registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_ROW: &str = "SELECT instance_suffix, policy_name, disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3";

/// crates/control/environment/src/environment/analyse.rs::read_policy registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_POLICY: &str = "SELECT readiness_budget_seconds, drain_bound_seconds \
               FROM registry.env_policies WHERE org = $1 AND name = $2";

/// crates/control/environment/src/environment/status.rs::stranded_runs registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_STATUS_STRANDED_RUNS: &str =
    "SELECT 1 FROM registry.environment_projects WHERE org = $1 AND project = $2 AND env = $3";

/// crates/control/environment/src/environment/delete.rs::delete_locked registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED: &str = "SELECT instance_suffix, policy_name FROM registry.environment_projects JOIN registry.environments USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3";

/// crates/control/environment/src/environment/bindings.rs::analyse registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_BINDINGS_ANALYSE: &str =
    "SELECT * FROM registry.resolve_environment_connection($1,$2,$3,$4,$5,$6)";

/// crates/control/environment/src/environment/sql.rs::read registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_SQL_READ: &str = "SELECT instance_suffix FROM registry.environment_projects \
             WHERE org=$1 AND project=$2 AND env=$3";

/// crates/control/environment/src/environment/delete/rows.rs::delete_registry_rows registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_ROWS_DELETE_REGISTRY_ROWS: &str =
    "DELETE FROM registry.environment_projects WHERE org = $1 AND project = $2 AND env = $3";

/// crates/control/package/src/integration.rs::publish registry statement.
pub const CONTROL_PACKAGE_SRC_INTEGRATION_PUBLISH: &str =
    "SELECT registry.publish_integration($1,$2,$3,$4::text::jsonb,$5,$6,$7)";

/// services/identity/src/session.rs::CURRENT_TARGET_SQL registry statement.
pub const IDENTITY_SRC_SESSION_CURRENT_TARGET_SQL: &str = "SELECT EXISTS (SELECT 1 FROM registry.environment_projects JOIN registry.environments USING (org,env) \
    WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4 \
    AND status = 'active')";

/// services/identity/src/session.rs::REGISTERED_ORG_SQL registry statement.
pub const IDENTITY_SRC_SESSION_REGISTERED_ORG_SQL: &str =
    "SELECT EXISTS (SELECT 1 FROM registry.orgs WHERE id = $1)";

/// Every project deployment affected by an environment lifecycle change.
pub const ENVIRONMENT_PROJECTS: &str = "SELECT project,env FROM registry.environment_projects WHERE org=$1 AND env=ANY($2) ORDER BY env,project";

/// Registered orgs whose resources belong to the supplied namespace.
pub const INSTALL_ORGS: &str = "SELECT id FROM registry.orgs WHERE id=$1 ORDER BY id";

/// Project coordinates, physical instance, org placement, pool, and policy name.
pub const INSTALL_PROJECTS: &str = "SELECT ep.org, ep.project, ep.env, ep.instance_suffix, \
    o.placement_type, o.pool_cluster, e.policy_name \
    FROM registry.environment_projects ep JOIN registry.orgs o ON o.id=ep.org \
    JOIN registry.environments e ON e.org=ep.org AND e.env=ep.env \
    WHERE ep.org=$1 ORDER BY ep.project, ep.env";
