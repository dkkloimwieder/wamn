//! SQL for core provisioning-saga state.
//!
//! The create-environment saga (`docs/plan/platform-ui.md` §5.2,
//! `wamn-zua8.3`) keeps its request types and the SQL text of each of its
//! records here. The `environment.create` route of the control host runs the
//! insert, and `wamn_control::environment_saga` runs the rest for
//! `wamn-ctl serve` (owner ruling of 2026-10-03). The copy-environment saga
//! (§5.3, `wamn-zua8.4`) uses the same records, and the
//! `environment.copy` route writes it.

use serde::{Deserialize, Serialize};
use wamn_catalog::RequirementType;

/// Create one provisioning saga idempotently.
pub fn create_saga_sql() -> &'static str {
    "INSERT INTO provisioning.sagas (saga_id, type, target, total_steps) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (saga_id) DO NOTHING"
}

/// Advance the durable provisioning checkpoint.
pub fn advance_saga_step_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET step = step + 1, status = 'running', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a provisioning saga complete.
pub fn complete_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'completed', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a provisioning saga failed and retain the diagnostic.
pub fn fail_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'failed', last_error = $2, updated_at = now() \
     WHERE saga_id = $1"
}

/// Read the durable provisioning checkpoint.
pub fn select_saga_sql() -> &'static str {
    "SELECT status, step, total_steps \
     FROM provisioning.sagas \
     WHERE saga_id = $1"
}

/// The saga type of the create-environment saga.
pub const CREATE_ENVIRONMENT: &str = "create-environment";

/// The steps of a create-environment saga, in the order the worker runs them
/// (owner rulings of 2026-10-03 on `wamn-zua8.3`). Step `n` is `STEPS[n - 1]`.
pub const STEPS: [&str; 15] = [
    "provision-project-env",
    "reconcile-run-plane",
    "prepare-credentials",
    "apply-packages",
    "reconcile-package-data-access",
    "enable-cdc",
    "wait-publication",
    "admit-components",
    "publish-release",
    "bind-connection",
    "push-release-manifest",
    "select-release",
    "upload-ui",
    "materialize-admin-grants",
    "awaiting-operator",
];

/// The request of one create-environment saga, kept in `input`. The
/// `environment.create` route takes exactly this shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequest {
    pub project: String,
    pub env: String,
    pub tenant: String,
    /// The hostname applied to every HTTP route of the release.
    pub route_host: String,
    /// Package artifacts that `push-package` pushed.
    pub packages: Vec<PackageReference>,
    pub connections: Vec<ConnectionRequest>,
}

/// One package artifact `<package_id>-<version>`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub package_id: String,
    pub version: String,
}

/// One connection that `bind-connection` binds to the store alias `alias`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    pub instance_id: String,
    pub requirement_type: RequirementType,
    pub alias: String,
    /// The non-secret definition, a JSON object.
    pub definition: serde_json::Value,
}

/// The saga type of the copy-environment saga.
pub const COPY_ENVIRONMENT: &str = "copy-environment";

/// The steps of a copy-environment saga (owner rulings of 2026-10-03 and
/// 2026-10-04 on `wamn-zua8.4`): `read-source`, then the steps of [`STEPS`]
/// with `copy-roles` before `select-release`.
pub const COPY_STEPS: [&str; 17] = [
    "read-source",
    "provision-project-env",
    "reconcile-run-plane",
    "prepare-credentials",
    "apply-packages",
    "reconcile-package-data-access",
    "enable-cdc",
    "wait-publication",
    "admit-components",
    "publish-release",
    "bind-connection",
    "push-release-manifest",
    "copy-roles",
    "select-release",
    "upload-ui",
    "materialize-admin-grants",
    "awaiting-operator",
];

/// The request of one copy-environment saga, kept in `input`. The
/// `environment.copy` route takes exactly this shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CopyRequest {
    pub project: String,
    /// The environment of the same project that the copy reads.
    pub source_env: String,
    pub env: String,
    pub tenant: String,
    /// The hostname applied to every HTTP route of the release.
    pub route_host: String,
    /// The new definitions. A source connection that this list does not
    /// name keeps its definition.
    pub connections: Vec<ConnectionReplacement>,
}

/// A new definition for the source connection `instance_id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionReplacement {
    pub instance_id: String,
    /// The non-secret definition, a JSON object.
    pub definition: serde_json::Value,
}

/// What `read-source` read from the source environment, with the
/// replacements of the request applied. The step keeps it in its `detail`,
/// and every later step of the copy reads it from there.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRead {
    /// The head release of the source environment.
    pub release: i32,
    pub packages: Vec<PackageReference>,
    pub connections: Vec<ConnectionRequest>,
}

impl CopyRequest {
    /// The request that the create steps of the copy run with.
    pub fn environment(&self, read: SourceRead) -> EnvironmentRequest {
        EnvironmentRequest {
            project: self.project.clone(),
            env: self.env.clone(),
            tenant: self.tenant.clone(),
            route_host: self.route_host.clone(),
            packages: read.packages,
            connections: read.connections,
        }
    }
}

/// The `target` of the create-environment saga of one environment.
pub fn environment_target(org: &str, project: &str, env: &str) -> String {
    format!("{org}/{project}/{env}")
}

/// Whether the project is a project of the org, and whether the environment
/// exists or has an open create or copy saga: `pending`, `running`, or
/// `failed`, which can resume. Parameters: org, project, env, the saga target.
pub fn create_environment_refusals_sql() -> &'static str {
    "SELECT EXISTS (SELECT 1 FROM registry.projects WHERE org = $1 AND id = $2), \
            EXISTS (SELECT 1 FROM registry.project_envs \
                     WHERE org = $1 AND project = $2 AND env = $3) \
         OR EXISTS (SELECT 1 FROM provisioning.sagas \
                     WHERE type IN ('create-environment', 'copy-environment') AND target = $4 \
                       AND status IN ('pending', 'running', 'failed'))"
}

/// Whether the environment exists in the project. Parameters: org,
/// project, env.
pub fn environment_exists_sql() -> &'static str {
    "SELECT EXISTS (SELECT 1 FROM registry.project_envs \
                     WHERE org = $1 AND project = $2 AND env = $3)"
}

/// The insert of one saga of type `$type` and all of its steps, `pending`,
/// that returns its new id.
macro_rules! saga_insert_sql {
    ($type:literal) => {
        concat!(
            "WITH saga AS ( \
               INSERT INTO provisioning.sagas (saga_id, type, target, total_steps, org, input) \
               VALUES (gen_random_uuid()::text, '",
            $type,
            "', $1, $2, $3, $4::text::jsonb) \
               RETURNING saga_id), \
             steps AS ( \
               INSERT INTO provisioning.saga_steps (saga_id, step, name) \
               SELECT saga.saga_id, s.step::int, s.name \
                 FROM saga, unnest($5::text[]) WITH ORDINALITY AS s (name, step)) \
             SELECT saga_id FROM saga"
        )
    };
}

/// Write one create-environment saga and all of its steps, `pending`, and
/// return its new id. Parameters: the saga target, the step count, org, the
/// request as JSON text, and the step names in order.
pub fn create_environment_saga_sql() -> &'static str {
    saga_insert_sql!("create-environment")
}

/// Write one copy-environment saga, with the parameters of
/// [`create_environment_saga_sql`].
pub fn copy_environment_saga_sql() -> &'static str {
    saga_insert_sql!("copy-environment")
}

/// The create and copy sagas of one project, oldest first: id, env, status,
/// last error, the steps as JSON text, type, and the source environment of a
/// copy. Parameters: org, project.
pub fn project_sagas_sql() -> &'static str {
    "SELECT s.saga_id, s.input->>'env', s.status, s.last_error, \
            (SELECT coalesce(jsonb_agg(jsonb_build_object( \
                        'step', t.step, 'name', t.name, 'status', t.status, \
                        'error', t.error, 'detail', t.detail, \
                        'started_at', t.started_at, 'finished_at', t.finished_at) \
                      ORDER BY t.step), '[]'::jsonb) \
               FROM provisioning.saga_steps t WHERE t.saga_id = s.saga_id)::text, \
            s.type, s.input->>'source_env' \
       FROM provisioning.sagas s \
      WHERE s.type IN ('create-environment', 'copy-environment') \
        AND s.org = $1 AND s.input->>'project' = $2 \
      ORDER BY s.created_at, s.saga_id"
}

/// Lock one create or copy saga and read its status. Parameter: saga id.
pub fn lock_environment_saga_sql() -> &'static str {
    "SELECT status FROM provisioning.sagas \
      WHERE saga_id = $1 AND type IN ('create-environment', 'copy-environment') FOR UPDATE"
}

/// Lock one create or copy saga of an org and read its status.
/// Parameters: saga id, org.
pub fn lock_org_environment_saga_sql() -> &'static str {
    "SELECT status FROM provisioning.sagas \
      WHERE saga_id = $1 AND type IN ('create-environment', 'copy-environment') \
        AND org = $2 FOR UPDATE"
}

/// Return a saga to `pending`. The worker starts its failed step again when
/// it claims the saga. Parameter: saga id.
pub fn resume_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
        SET status = 'pending', last_error = NULL, updated_at = now() \
      WHERE saga_id = $1"
}

/// End a saga as `abandoned`. Parameter: saga id.
pub fn abandon_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas SET status = 'abandoned', updated_at = now() \
      WHERE saga_id = $1"
}

/// The oldest `pending` or `running` create or copy saga of an org with no
/// other running saga: its id, org, request, first step that is not
/// `completed`, and type.
pub fn next_open_saga_sql() -> &'static str {
    "SELECT s.saga_id, s.org, s.input, \
            (SELECT min(t.step) FROM provisioning.saga_steps t \
              WHERE t.saga_id = s.saga_id AND t.status <> 'completed'), \
            s.type \
       FROM provisioning.sagas s \
      WHERE s.type IN ('create-environment', 'copy-environment') \
        AND s.status IN ('pending', 'running') \
        AND NOT EXISTS ( \
            SELECT FROM provisioning.sagas o \
             WHERE o.org = s.org AND o.saga_id <> s.saga_id AND o.status = 'running') \
      ORDER BY s.created_at, s.saga_id \
      LIMIT 1"
}

/// Mark a step `running` and its saga `running` at that step. Parameters:
/// saga id, step.
pub fn start_step_sql() -> &'static str {
    "WITH saga AS ( \
       UPDATE provisioning.sagas SET status = 'running', step = $2, updated_at = now() \
        WHERE saga_id = $1) \
     UPDATE provisioning.saga_steps \
        SET status = 'running', error = NULL, started_at = now(), finished_at = NULL \
      WHERE saga_id = $1 AND step = $2"
}

/// Mark a step `completed`. Parameters: saga id, step, detail (null when the
/// step reports none).
pub fn complete_step_sql() -> &'static str {
    "UPDATE provisioning.saga_steps SET status = 'completed', detail = $3, finished_at = now() \
      WHERE saga_id = $1 AND step = $2"
}

/// Mark a step `failed` with its error, and its saga `failed`. Parameters:
/// saga id, step, error.
pub fn fail_step_sql() -> &'static str {
    "WITH saga AS ( \
       UPDATE provisioning.sagas SET status = 'failed', last_error = $3, updated_at = now() \
        WHERE saga_id = $1) \
     UPDATE provisioning.saga_steps \
        SET status = 'failed', error = $3, finished_at = now() \
      WHERE saga_id = $1 AND step = $2"
}

/// The detail of one completed step. Parameters: saga id, step.
pub fn step_detail_sql() -> &'static str {
    "SELECT detail FROM provisioning.saga_steps \
      WHERE saga_id = $1 AND step = $2 AND status = 'completed'"
}

/// Complete the last step with the operator commands in `detail`, and leave
/// the saga `awaiting-operator`. Parameters: saga id, step, detail.
pub fn await_operator_sql() -> &'static str {
    "WITH saga AS ( \
       UPDATE provisioning.sagas SET status = 'awaiting-operator', updated_at = now() \
        WHERE saga_id = $1) \
     UPDATE provisioning.saga_steps \
        SET status = 'completed', detail = $3, finished_at = now() \
      WHERE saga_id = $1 AND step = $2"
}
