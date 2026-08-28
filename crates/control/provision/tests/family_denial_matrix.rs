//! THE PER-FAMILY DENIAL MATRIX (`wamn-0h0g.22.15`).
//!
//! For every ORDERED PAIR of authority families, a principal of one family is
//! REFUSED the operations that belong to the others. The arms are per-family so
//! a red NAMES the family that over-holds; they compose into this one gate
//! binary.
//!
//! # Everything here is the server's own answer
//!
//! `has_table_privilege`, `has_any_column_privilege`, `aclexplode`, `pg_policy`,
//! `pg_index`, `pg_get_functiondef` and real SQLSTATEs carried back into Rust.
//! Never the text of a checked-in statement: this crate EMITS much of that text,
//! so a text assertion would be a builder checked against itself, and a text
//! assertion cannot tell a live declaration from a comment that mentions one.
//!
//! # Why a privilege probe alone is not enough
//!
//! `has_table_privilege` IS BLIND TO RLS. It answers TRUE for a role whose every
//! `SELECT` returns zero rows, so it cannot tell an isolated credential from a
//! locked-out one — and under `FORCE ROW LEVEL SECURITY` a principal matching no
//! policy reads ZERO ROWS WITH NO ERROR. Every arm whose claim is about RLS
//! REACH therefore asserts the POST-STATE ROW COUNT from a MINTED GENERATION
//! LOGIN, never from the bare stable ACL role — which derives a NULL tenant key
//! and matches nothing silently — and keeps a control arm
//! ([`the_effect_writer_arm_reaches_exactly_its_four_run_plane_ledgers`]'s
//! outsider) that legitimately reads zero.
//!
//! # Hermetic, and it owns its server
//!
//! Roles are CLUSTER-WIDE and `deploy/sql/postgres-init.sql` carries a bare
//! `CREATE DATABASE wamn`, so this gate rebuilds its whole world and must be
//! pointed only at a disposable container. ONE FRESH CLUSTER PER ROLE OR GRANT
//! MUTANT.
//!
//! ```bash
//! docker run -d --name w68-matrix-pg -e POSTGRES_PASSWORD=probe \
//!   -p 127.0.0.1:5443:5432 postgres:18
//! # THE ONLY HONEST READINESS PROBE IS A CONNECT FROM THE HOST OVER THE
//! # MAPPED PORT: pg_isready, a TCP connect and `docker exec psql` all answer
//! # yes while the published port is still down.
//! until psql postgres://postgres:probe@127.0.0.1:5443/postgres -Atqc 'select 1'; do :; done
//! WAMN_DENIAL_MATRIX_PG_URL=postgres://postgres:probe@127.0.0.1:5443/postgres \
//!   cargo test -p wamn-control-provision --test family_denial_matrix -- --test-threads=1
//! docker rm -f w68-matrix-pg      # BY EXPLICIT NAME. Never prune.
//! ```

use std::collections::BTreeSet;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use url::Url;

use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{
    PLATFORM_GROUP_ROLE, WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind,
    workload_generation_role,
};

const POSTGRES_INIT: &str = include_str!("../../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

const ENV_VAR: &str = "WAMN_DENIAL_MATRIX_PG_URL";

/// The database `deploy/sql/postgres-init.sql` creates. Also the scope every
/// generation login's digest is taken over, so it cannot be chosen freely.
const DATABASE: &str = "wamn";

const ORG: &str = "acme";
const PROJECT: &str = "billing";
const ENVIRONMENT: &str = "dev";

/// The tenant every tenant-scoped generation in this gate is minted for, and
/// the tenant the guest may read. `TENANT_B` is the neighbour it may not.
const TENANT_A: &str = "t1";
const TENANT_B: &str = "t2";

/// Throwaway password for the probe logins. Not a credential of record: the
/// gate builds and drops these roles inside a disposable container.
const PROBE_PASSWORD: &str = "w68-matrix-probe";

/// A login in NEITHER `wamn_platform` nor any family, holding the SAME table
/// grant as the effect writer. It is the control that makes the writer's row
/// counts mean something: under FORCE RLS this one reads ZERO ROWS WITH NO
/// ERROR, which is exactly what a silently locked-out family looks like.
const OUTSIDER: &str = "wamn_matrix_outsider";

/// The reserved host-only group that is NOT a [`WorkloadRoleFamily`] and has no
/// platform-group exception.
const SCENARIO_AUTHOR_ROLE: &str = "wamn_scenario_author";

/// A test-only login inheriting [`SCENARIO_AUTHOR_ROLE`].
///
/// Production deliberately mints no such credential. This probe exists only to
/// carry the role's effective authority through a real connection and prove
/// both residue reads receive SQLSTATE 42501.
const SCENARIO_AUTHOR_PROBE: &str = "wamn_matrix_author_probe";

// ---------------------------------------------------------------------------
// THE MATRIX UNIVERSE
// ---------------------------------------------------------------------------

/// Every relation any matrix family holds, or could plausibly be widened onto.
///
/// The denial matrix is a claim about THESE objects: the run plane plus the
/// catalog relations the platform families read. A family reaching one it does
/// not own is what the pairwise arms below name.
const MATRIX_RELATIONS: [&str; 18] = [
    "catalog.catalog_heads",
    "catalog.component_library",
    "catalog.connection_bindings",
    "catalog.connection_generations",
    "catalog.connection_instances",
    "catalog.connection_requirements",
    "catalog.release_components",
    "catalog.release_manifest_v2_snapshots",
    "catalog.wiring_activation",
    "catalog.wiring_tombstones",
    "catalog.wirings",
    "wamn_run.effect_attempt_dispatches",
    "wamn_run.effect_attempt_outcomes",
    "wamn_run.effect_attempts",
    "wamn_run.environment_policies",
    "wamn_run.operator_run_actions",
    "wamn_run.run_queue",
    "wamn_run.runs",
];

const MATRIX_PRIVILEGES: [&str; 4] = ["SELECT", "INSERT", "UPDATE", "DELETE"];

/// The authority-bearing routines. `catalog`'s two trigger functions are
/// deliberately absent: they are invoker-rights triggers, not a family's
/// authority.
const MATRIX_ROUTINES: [&str; 4] = [
    "wamn_authority.current_tenant_key()",
    "wamn_authority.tenant_key(text)",
    "wamn_run.require_executor_platform_authority()",
    "wamn_run.require_management_admission_authority()",
];

/// One family's REACH over [`MATRIX_RELATIONS`] and [`MATRIX_ROUTINES`], spelled
/// as LITERALS.
///
/// # Why literals and not a call to the builder
///
/// A value asserted against the constant that produced it is a tautology, and
/// this branch has paid for that shape before. These rows are the SECOND
/// DOCUMENT: the first is what PostgreSQL answers after the real artifacts and
/// the real `sql::stable_surface_sql` batches have been applied. Widening a
/// family — in a builder or in `deploy/sql/run-state.sql` — moves the server's
/// answer and not these rows, so the arm reds.
struct FamilyReach {
    family: WorkloadRoleFamily,
    /// `schema.relation|PRIVILEGE|grain`, sorted. `grain` is `table` when the
    /// grant covers the whole relation and `column` when only some columns carry
    /// it. The grain is pinned because a column-exact grant quietly becoming a
    /// table grant is a widening the coarse form cannot see.
    relations: &'static [&'static str],
    /// Routines whose `proacl` names the family's STABLE ACL role explicitly.
    ///
    /// `has_function_privilege` is NOT the probe here. `deploy/sql/run-state.sql`
    /// never revokes PUBLIC on the two `require_*` guards, so that function
    /// answers TRUE for every role in the cluster and cannot distinguish one
    /// family from another. `aclexplode(p.proacl)` can.
    routines: &'static [&'static str],
}

/// The nine families whose credentials reach a project-environment database.
///
/// `ControlAuthor`, `RegistryReader` and `IdentityReader` are absent because
/// their scope is [`WorkloadRoleScopeKind::Control`]: their credentials reach
/// the CONTROL database, whose grants and whose authority derivation are a
/// different plane. `wamn_scenario_author` is absent because it is a host group,
/// not a [`WorkloadRoleFamily`] — it has no generation lifecycle to mint a
/// principal from.
const MATRIX: [FamilyReach; 9] = [
    FamilyReach {
        family: WorkloadRoleFamily::App,
        relations: &[
            "catalog.catalog_heads|SELECT|table",
            "catalog.component_library|SELECT|table",
            "catalog.connection_bindings|SELECT|table",
            "catalog.connection_generations|SELECT|table",
            "catalog.connection_instances|SELECT|table",
            "catalog.connection_requirements|SELECT|table",
            "catalog.release_components|SELECT|table",
            "catalog.release_manifest_v2_snapshots|SELECT|table",
            "catalog.wiring_activation|SELECT|table",
            "catalog.wiring_tombstones|SELECT|table",
            "catalog.wirings|SELECT|table",
            "wamn_run.effect_attempt_dispatches|SELECT|table",
            "wamn_run.effect_attempt_outcomes|SELECT|table",
            "wamn_run.effect_attempts|SELECT|table",
            "wamn_run.environment_policies|SELECT|table",
            "wamn_run.runs|DELETE|table",
            "wamn_run.runs|SELECT|table",
        ],
        routines: &[
            "wamn_authority.current_tenant_key()",
            "wamn_authority.tenant_key(text)",
        ],
    },
    FamilyReach {
        family: WorkloadRoleFamily::EffectWriter,
        relations: &[
            "wamn_run.effect_attempt_dispatches|SELECT|table",
            "wamn_run.effect_attempt_outcomes|SELECT|table",
            "wamn_run.effect_attempts|SELECT|table",
            "wamn_run.run_queue|SELECT|column",
            "wamn_run.runs|SELECT|column",
        ],
        routines: &[],
    },
    FamilyReach {
        family: WorkloadRoleFamily::Retention,
        relations: &["wamn_run.runs|DELETE|table", "wamn_run.runs|SELECT|column"],
        routines: &[],
    },
    FamilyReach {
        family: WorkloadRoleFamily::ManagementAdmitter,
        relations: &[
            "catalog.component_library|SELECT|table",
            "catalog.connection_bindings|SELECT|table",
            "catalog.connection_generations|SELECT|table",
            "catalog.connection_instances|SELECT|table",
            "catalog.connection_requirements|SELECT|table",
            "catalog.wirings|SELECT|table",
            "wamn_run.environment_policies|SELECT|table",
            "wamn_run.run_queue|INSERT|column",
            "wamn_run.run_queue|SELECT|column",
            "wamn_run.runs|INSERT|column",
            "wamn_run.runs|SELECT|column",
        ],
        routines: &["wamn_authority.tenant_key(text)"],
    },
    FamilyReach {
        family: WorkloadRoleFamily::ExecutorPlatform,
        relations: &[
            "catalog.catalog_heads|SELECT|table",
            "catalog.component_library|SELECT|table",
            "catalog.connection_bindings|SELECT|table",
            "catalog.connection_generations|SELECT|table",
            "catalog.connection_instances|SELECT|table",
            "catalog.connection_requirements|SELECT|table",
            "catalog.release_components|SELECT|table",
            "catalog.release_manifest_v2_snapshots|SELECT|table",
            "catalog.wiring_activation|SELECT|table",
            "catalog.wiring_tombstones|SELECT|table",
            "catalog.wirings|SELECT|table",
            "wamn_run.effect_attempts|SELECT|table",
            "wamn_run.run_queue|DELETE|table",
            "wamn_run.run_queue|SELECT|table",
            "wamn_run.run_queue|UPDATE|column",
            "wamn_run.runs|SELECT|table",
            "wamn_run.runs|UPDATE|column",
        ],
        routines: &[
            "wamn_authority.tenant_key(text)",
            "wamn_run.require_executor_platform_authority()",
        ],
    },
    FamilyReach {
        family: WorkloadRoleFamily::HttpAdmitter,
        relations: &[
            "catalog.component_library|SELECT|table",
            "catalog.connection_bindings|SELECT|table",
            "catalog.connection_generations|SELECT|table",
            "catalog.connection_instances|SELECT|table",
            "catalog.connection_requirements|SELECT|table",
            "catalog.wirings|SELECT|table",
        ],
        routines: &[],
    },
    FamilyReach {
        family: WorkloadRoleFamily::DispatchReader,
        relations: &[
            "wamn_run.effect_attempts|SELECT|table",
            "wamn_run.run_queue|SELECT|table",
        ],
        routines: &[],
    },
    // The two families whose MEASURED surface is empty (`wamn-0h0g.22.37`).
    // They are in the matrix precisely because they hold nothing: they are the
    // sharpest subjects of the pairwise arms, and the day one of them acquires
    // a grant the exactness arm reds instead of the widening shipping quietly.
    FamilyReach {
        family: WorkloadRoleFamily::ServiceReader,
        relations: &[],
        routines: &[],
    },
    FamilyReach {
        family: WorkloadRoleFamily::EventMaterializer,
        relations: &[],
        routines: &[],
    },
];

// ---------------------------------------------------------------------------
// psql
// ---------------------------------------------------------------------------

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal is asserted on `42501` rather than on an error string.
fn psql(url: &str, script: &str) -> (bool, String, String) {
    let mut child = Command::new("psql")
        .arg(url)
        .args([
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
            "-q",
            "-f",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write the script to psql");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn apply(url: &str, what: &str, script: &str) {
    let (ok, out, err) = psql(url, script);
    assert!(ok, "{what} failed:\nstdout:\n{out}\nstderr:\n{err}");
}

fn query(url: &str, sql: &str) -> String {
    let output = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-tAq", "-c", sql])
        .output()
        .expect("spawn psql");
    assert!(
        output.status.success(),
        "query failed:\n{sql}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn rows(url: &str, sql: &str) -> Vec<String> {
    query(url, sql)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The SQLSTATE one statement raised as the given login, or `None` on success.
fn sqlstate(url: &str, statement: &str) -> Option<String> {
    let (ok, _, err) = psql(url, statement);
    if ok {
        return None;
    }
    // `VERBOSITY=verbose` prints `ERROR:  42501: permission denied for …`. A
    // SQLSTATE is five uppercase alphanumerics carrying at least one digit,
    // which is what separates it from the `ERROR:` label beside it.
    err.split_whitespace()
        .filter_map(|token| token.strip_suffix(':'))
        .find(|code| {
            code.len() == 5
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && code.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .or_else(|| panic!("no SQLSTATE in psql stderr:\n{err}"))
}

/// The admin URL with its userinfo replaced by one role's login, pointed at the
/// project database.
fn login_url(admin_url: &str, role: &str) -> String {
    let mut url = Url::parse(admin_url).unwrap_or_else(|_| panic!("{ENV_VAR} is a URL"));
    url.set_username(role).expect("set the probe role");
    url.set_password(Some(PROBE_PASSWORD))
        .expect("set password");
    url.set_path(DATABASE);
    url.into()
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    admin: String,
    db_url: String,
}

/// The deterministic generation name the real mint would issue for one family.
fn generation(family: WorkloadRoleFamily) -> String {
    let scope = match family.scope_kind() {
        WorkloadRoleScopeKind::Tenant => WorkloadRoleScope::Tenant {
            tenant: TENANT_A,
            database: DATABASE,
        },
        WorkloadRoleScopeKind::ProjectEnvironment => WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: DATABASE,
        },
        WorkloadRoleScopeKind::Control => panic!(
            "{family:?} is control-scoped: its credential reaches the control \
             database and it is not a subject of this matrix"
        ),
    };
    workload_generation_role(family, scope, CredentialGeneration::A)
        .unwrap_or_else(|error| panic!("mint a {family:?} generation: {error}"))
}

/// Roles this gate creates, drops or re-mints. Roles are CLUSTER-WIDE.
fn managed_roles() -> Vec<String> {
    let mut roles: Vec<String> = MATRIX
        .iter()
        .flat_map(|reach| [reach.family.acl_role().to_owned(), generation(reach.family)])
        .collect();
    // The probe comes BEFORE the group it inherits: a role still named by a
    // `pg_auth_members` row cannot be dropped.
    roles.push(SCENARIO_AUTHOR_PROBE.to_owned());
    roles.push(SCENARIO_AUTHOR_ROLE.to_owned());
    roles.push(PLATFORM_GROUP_ROLE.to_owned());
    roles.push(OUTSIDER.to_owned());
    roles
}

/// Rebuild the whole world from scratch.
///
/// HERMETIC ON PURPOSE, and it must FAIL on a dirty cluster rather than repair
/// one quietly: `postgres-init.sql` carries bare `CREATE ROLE`s, so a leftover
/// HEALTHY role would satisfy an `IF NOT EXISTS` elsewhere and MASK a mutated
/// builder. `DROP OWNED BY` precedes `DROP ROLE`, inside an existence check, and
/// the DATABASE goes first — `DROP OWNED BY` reaches only the current database,
/// and a role still named by a `relacl` cannot be dropped.
fn reset(admin: &str) {
    apply(
        admin,
        "drop the project database",
        "DROP DATABASE IF EXISTS \"wamn\";\n",
    );
    for role in managed_roles() {
        apply(
            admin,
            "drop a managed role",
            &format!(
                "DO $reset$ BEGIN \
                   IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                     EXECUTE 'DROP OWNED BY \"{role}\"'; \
                     EXECUTE 'DROP ROLE \"{role}\"'; \
                   END IF; \
                 END $reset$;\n"
            ),
        );
    }
    // The stock image leaves PUBLIC connectable, which would let every probe
    // login below reach a database its family was never provisioned for.
    apply(
        admin,
        "close the stock PUBLIC connect floor",
        "REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n\
         REVOKE CONNECT ON DATABASE template1 FROM PUBLIC;\n",
    );
}

/// Two tenants' worth of rows in every relation an RLS-reach arm reads back.
///
/// Two, not one: an arm that reads its own tenant cannot tell confinement from
/// an empty table, and the platform arm's whole claim is that it reads BOTH.
const SEED: &str = "\
DO $seed$ DECLARE t text; a uuid; BEGIN
FOREACH t IN ARRAY ARRAY['t1','t2'] LOOP
  INSERT INTO catalog.catalogs (tenant_id, catalog_id, version, environment, schema_version, state)
    VALUES (t, 'cat', 1, 'dev', '0.1', 'applied');
  INSERT INTO catalog.releases (tenant_id, catalog_id, catalog_version) VALUES (t, 'cat', 1);
  INSERT INTO catalog.catalog_heads (tenant_id, catalog_id, environment, applied_catalog_version)
    VALUES (t, 'cat', 'dev', 1);
  INSERT INTO wamn_run.environment_policies (tenant_id, expected_environment, durability_class)
    VALUES (t, 'dev', 'standard');
  INSERT INTO wamn_run.runs
    (tenant_id, run_id, catalog_id, catalog_version, environment, status, trigger_source,
     wiring_id, wiring_version, wiring_hash, binding_world_json, input_json)
    VALUES (t, 'r1', 'cat', 1, 'dev', 'dispatched', 'internal', 'w', 1,
            'sha256:'||repeat('c',64), '[]', '{\"a\":1}');
  a := gen_random_uuid();
  INSERT INTO wamn_run.effect_attempts
    (tenant_id, attempt_id, run_id, root_plan_hash, current_plan_hash, local_node_id,
     source_artifact_hash, requirement_name, occurrence, seq, generation_fact_kind,
     attempt_started_at, attempt_deadline_at, attempt_input_ref)
    VALUES (t, a, 'r1', 'sha256:'||repeat('a',64), 'sha256:'||repeat('b',64), 'n1',
            'sha256:'||repeat('d',64), 'req', 0, 0, 'not-required',
            now(), now() + interval '1 hour', 'ref');
  INSERT INTO wamn_run.effect_attempt_dispatches
    (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id,
     occurrence, dispatched_at)
    SELECT t, a, e.attempt_started_at, 'r1', 0, 'n1', 0, e.attempt_started_at
      FROM wamn_run.effect_attempts e WHERE e.tenant_id = t AND e.attempt_id = a;
  INSERT INTO wamn_run.effect_attempt_outcomes
    (tenant_id, attempt_id, dispatched_at, outcome_status, recorded_at)
    SELECT t, a, d.dispatched_at, 'success', d.dispatched_at
      FROM wamn_run.effect_attempt_dispatches d WHERE d.tenant_id = t AND d.attempt_id = a;
END LOOP;
END $seed$;
";

static FIXTURE: OnceLock<Fixture> = OnceLock::new();

/// Build the world once per process: the real artifacts, the real convergent
/// grant batches, and one minted generation login per family.
fn build(admin: String) -> Fixture {
    reset(&admin);
    apply(&admin, "apply postgres-init.sql", POSTGRES_INIT);
    // The admin URL keeps its own userinfo and moves to the project database;
    // `login_url` is for the probe logins.
    let mut admin_db = Url::parse(&admin).unwrap_or_else(|_| panic!("{ENV_VAR} is a URL"));
    admin_db.set_path(DATABASE);
    let db_url = admin_db.to_string();

    for artifact in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, "apply a schema artifact", artifact);
    }
    apply(&db_url, "seed two tenants", SEED);

    for reach in &MATRIX {
        mint(&db_url, reach.family);
    }
    // The control principal: SAME table grant as the effect writer, member of
    // nothing. It exists to prove that a row count of zero is what a lockout
    // looks like, so a writer arm reading rows is a real result.
    apply(
        &db_url,
        "mint the outsider control login",
        &format!(
            "CREATE ROLE {OUTSIDER} LOGIN PASSWORD '{PROBE_PASSWORD}' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS;\n\
             GRANT CONNECT ON DATABASE \"{DATABASE}\" TO {OUTSIDER};\n\
             GRANT USAGE ON SCHEMA wamn_run TO {OUTSIDER};\n\
             GRANT SELECT ON wamn_run.effect_attempts TO {OUTSIDER};\n"
        ),
    );
    // A test-only login that INHERITS the scenario-author group. It adds no
    // privilege of its own and does not make the group a member of anything, so
    // every observed read comes from that reserved role alone.
    apply(
        &db_url,
        "mint the scenario-author probe login",
        &format!(
            "CREATE ROLE {SCENARIO_AUTHOR_PROBE} LOGIN PASSWORD '{PROBE_PASSWORD}' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS;\n\
             GRANT {SCENARIO_AUTHOR_ROLE} TO {SCENARIO_AUTHOR_PROBE} \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;\n\
             GRANT CONNECT ON DATABASE \"{DATABASE}\" TO {SCENARIO_AUTHOR_PROBE};\n"
        ),
    );

    Fixture { admin, db_url }
}

/// Prepare one family's stable surface and one generation login.
///
/// # The guest is hand-minted, and that is a measured fact rather than a taste
///
/// `sql::prepare_workload_generation_sql` composes
/// `ensure_workload_acl_role_sql`, which HARDENS its target to
/// `NOLOGIN PASSWORD NULL NOINHERIT`. `deploy/sql/postgres-init.sql` creates
/// `wamn_app` as `LOGIN PASSWORD 'wamn_app'` with `INHERIT`. Measured on
/// PostgreSQL 18.6 against these very artifacts: running the ensure batch over
/// `wamn_app` flips it to `NOLOGIN PASSWORD NULL NOINHERIT`. Using the generic
/// prepare for the guest here would therefore have this gate mutate the very
/// role it is measuring. The membership edge is spelled exactly as the
/// production builder spells it — `WITH ADMIN FALSE, INHERIT TRUE, SET FALSE` —
/// because PostgreSQL 16+ takes a new membership's `INHERIT` default from the
/// MEMBER's `rolinherit`, and a bare `GRANT` onto a `NOINHERIT` role lands
/// `inherit_option = false`, which reads ZERO ROWS with no error.
fn mint(db_url: &str, family: WorkloadRoleFamily) {
    let login = generation(family);
    if family == WorkloadRoleFamily::App {
        apply(
            db_url,
            "mint the guest generation",
            &format!(
                "CREATE ROLE \"{login}\" LOGIN PASSWORD '{PROBE_PASSWORD}' \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS;\n\
                 GRANT {role} TO \"{login}\" WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;\n\
                 GRANT CONNECT ON DATABASE \"{DATABASE}\" TO \"{login}\";\n",
                role = family.acl_role(),
            ),
        );
        return;
    }
    apply(
        db_url,
        "prepare a workload generation",
        &sql::prepare_workload_generation_sql(
            family,
            DATABASE,
            &login,
            PROBE_PASSWORD,
            "2099-01-01T00:00:00Z",
        ),
    );
    // The dispatcher's in-database read surface is not part of
    // `stable_surface_sql`; it is applied by the provisioner as its own step.
    if family == WorkloadRoleFamily::DispatchReader {
        apply(
            db_url,
            "converge the dispatch-reader read surface",
            &sql::grant_dispatch_reader_read_surface_sql("wamn_run"),
        );
    }
}

/// `None` and a printed skip line when the gate is not armed.
///
/// A self-skipping env-gated test reports PASS and has never executed, so the
/// line names the test that skipped.
fn armed(test: &str) -> Option<&'static Fixture> {
    let Ok(admin) = std::env::var(ENV_VAR) else {
        eprintln!("skipping {test} (set {ENV_VAR} to run)");
        return None;
    };
    Some(FIXTURE.get_or_init(|| build(admin)))
}

// ---------------------------------------------------------------------------
// The reach the SERVER reports
// ---------------------------------------------------------------------------

fn sql_array(values: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let items: Vec<String> = values
        .into_iter()
        .map(|value| format!("'{}'", value.as_ref()))
        .collect();
    format!("ARRAY[{}]", items.join(", "))
}

/// What the named principal can actually reach over the matrix universe.
///
/// `has_any_column_privilege` is asked for every privilege that HAS a column
/// grain; PostgreSQL rejects `DELETE` there, so the `CASE` — which evaluates in
/// order, unlike `AND` — keeps that ask off the wire rather than relying on
/// short-circuiting.
///
/// Every ordering in this file is `COLLATE "C"`. The server's default collation
/// ignores `_`, so `catalog.wirings` sorts BEFORE `catalog.wiring_tombstones`
/// there and a pinned list would be ordered by the container's locale rather
/// than by its bytes.
fn observed_relations(db_url: &str, role: &str) -> Vec<String> {
    rows(
        db_url,
        &format!(
            "SELECT row FROM ( \
               SELECT rel || '|' || priv || '|' || \
                      CASE WHEN has_table_privilege('{role}', rel, priv) \
                           THEN 'table' ELSE 'column' END AS row \
                 FROM unnest({relations}) AS rel, unnest({privileges}) AS priv \
                WHERE has_table_privilege('{role}', rel, priv) \
                   OR CASE WHEN priv = 'DELETE' THEN false \
                           ELSE has_any_column_privilege('{role}', rel, priv) END \
             ) q ORDER BY row COLLATE \"C\"",
            relations = sql_array(MATRIX_RELATIONS),
            privileges = sql_array(MATRIX_PRIVILEGES),
        ),
    )
}

/// The routines whose `proacl` names this family's stable role explicitly.
///
/// The signature is rebuilt from `proargtypes` rather than taken from
/// `pg_get_function_identity_arguments`, which renders the PARAMETER NAME beside
/// the type for a SQL-language function and so would not match a signature
/// spelled by type.
fn observed_routines(db_url: &str, acl_role: &str) -> Vec<String> {
    rows(
        db_url,
        &format!(
            "SELECT row FROM ( \
               SELECT n.nspname || '.' || p.proname || '(' \
                      || coalesce((SELECT string_agg(format_type(t, NULL), ', ' ORDER BY ord) \
                                     FROM unnest(p.proargtypes) \
                                          WITH ORDINALITY AS a(t, ord)), '') \
                      || ')' AS row \
                 FROM pg_catalog.pg_proc p \
                 JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                 CROSS JOIN LATERAL aclexplode(p.proacl) x \
                WHERE x.grantee = '{acl_role}'::regrole \
                  AND x.privilege_type = 'EXECUTE' \
             ) q WHERE row = ANY({routines}) ORDER BY row COLLATE \"C\"",
            routines = sql_array(MATRIX_ROUTINES),
        ),
    )
}

/// One family's pinned capabilities, GRAIN-FREE: `relation|PRIVILEGE` and
/// `routine|signature`. Reach is reach — the pairwise arms are about whether a
/// principal can perform an operation at all.
fn pinned_capabilities(reach: &FamilyReach) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for row in reach.relations {
        let (capability, _grain) = row
            .rsplit_once('|')
            .unwrap_or_else(|| panic!("a pinned relation row carries a grain: {row}"));
        set.insert(capability.to_owned());
    }
    for routine in reach.routines {
        set.insert(format!("routine|{routine}"));
    }
    set
}

fn family(subject: WorkloadRoleFamily) -> &'static FamilyReach {
    MATRIX
        .iter()
        .find(|reach| reach.family == subject)
        .unwrap_or_else(|| panic!("{subject:?} is not a matrix family"))
}

/// One family's own row of the matrix, plus the ordered pairs it is the SUBJECT
/// of.
///
/// Two claims, and neither subsumes the other. The exactness half catches a
/// widening onto an object NO family owns; the pairwise half NAMES the family
/// whose operation was taken.
fn assert_family_row(subject: WorkloadRoleFamily) {
    let Some(fixture) = armed(&format!("the denial matrix row for {subject:?}")) else {
        return;
    };
    let reach = family(subject);
    let login = generation(subject);
    let label = subject.label();

    // The principal must be able to authenticate and must not be able to see
    // past RLS, or every denial below is satisfied for the wrong reason.
    assert_eq!(
        query(
            &fixture.db_url,
            &format!(
                "SELECT (rolcanlogin AND NOT rolsuper AND NOT rolbypassrls)::text \
                   FROM pg_roles WHERE rolname = '{login}'"
            ),
        ),
        "true",
        "the {label} generation is not a usable unprivileged login: a superuser \
         or BYPASSRLS probe satisfies every arm below while proving nothing"
    );

    // --- 1. THE ORDERED PAIRS THIS FAMILY IS THE SUBJECT OF -----------------
    //
    // FIRST, deliberately. This is the arm that names the VICTIM family, and a
    // widening trips both arms — so running exactness first would shadow the
    // more informative message behind a diff of two long lists.
    let mine = pinned_capabilities(reach);
    let held = observed_capabilities(&fixture.db_url, &login, subject.acl_role());
    for other in &MATRIX {
        if other.family == subject {
            continue;
        }
        for capability in pinned_capabilities(other).difference(&mine) {
            assert!(
                !held.contains(capability),
                "DENIAL MATRIX: the {label} family REACHES {capability}, which \
                 belongs to the {victim} family and not to it",
                victim = other.family.label(),
            );
        }
    }

    // --- 2. EXACTNESS ------------------------------------------------------
    //
    // What the pairwise arm cannot see: a widening onto an object NO family
    // owns, and an under-holding that silently strands this family's own reader.
    assert_eq!(
        observed_relations(&fixture.db_url, &login),
        reach.relations,
        "the {label} family's relation reach is not its pinned surface — it \
         over-holds or under-holds somewhere in the matrix universe"
    );
    assert_eq!(
        observed_routines(&fixture.db_url, subject.acl_role()),
        reach.routines,
        "the {label} family's routine grants are not its pinned surface"
    );
}

/// Everything the principal reaches, grain-free, in the same vocabulary as
/// [`pinned_capabilities`].
fn observed_capabilities(db_url: &str, login: &str, acl_role: &str) -> BTreeSet<String> {
    let mut held: BTreeSet<String> = observed_relations(db_url, login)
        .into_iter()
        .map(|row| {
            row.rsplit_once('|')
                .expect("an observed relation row carries a grain")
                .0
                .to_owned()
        })
        .collect();
    held.extend(
        observed_routines(db_url, acl_role)
            .into_iter()
            .map(|routine| format!("routine|{routine}")),
    );
    held
}

// ---------------------------------------------------------------------------
// THE MATRIX, one arm per family so a red names the family
// ---------------------------------------------------------------------------

#[test]
fn the_guest_sql_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::App);
}

#[test]
fn the_effect_writer_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::EffectWriter);
}

#[test]
fn the_retention_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::Retention);
}

#[test]
fn the_management_admitter_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::ManagementAdmitter);
}

#[test]
fn the_executor_platform_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::ExecutorPlatform);
}

#[test]
fn the_http_admitter_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::HttpAdmitter);
}

#[test]
fn the_dispatch_reader_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::DispatchReader);
}

#[test]
fn the_service_reader_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::ServiceReader);
}

#[test]
fn the_event_materializer_family_is_refused_the_other_families_operations() {
    assert_family_row(WorkloadRoleFamily::EventMaterializer);
}

/// The matrix is PAIRWISE, and this is what makes that literally true.
///
/// A pure arm, so it fails in the ordinary sweep and not only against a server:
/// every ordered pair is covered, no family is listed twice, and no family's
/// pinned surface is a superset of every other's — which would make its own row
/// vacuous.
#[test]
fn every_ordered_pair_of_matrix_families_is_covered_exactly_once() {
    let mut seen = BTreeSet::new();
    for reach in &MATRIX {
        assert!(
            seen.insert(reach.family.acl_role()),
            "{:?} appears twice in the matrix",
            reach.family
        );
        assert!(
            !matches!(reach.family.scope_kind(), WorkloadRoleScopeKind::Control),
            "{:?} is control-scoped and reaches a different database plane",
            reach.family
        );
    }
    assert_eq!(seen.len(), MATRIX.len());

    let empty: Vec<String> = MATRIX
        .iter()
        .filter(|reach| pinned_capabilities(reach).is_empty())
        .map(|reach| reach.family.label())
        .collect();
    assert_eq!(
        empty,
        vec!["service-reader".to_owned(), "event-materializer".to_owned()],
        "the set of families whose MEASURED surface is empty moved; every \
         ordered pair naming one of them as the OBJECT carries no denial by \
         construction"
    );

    let mut pairs = 0usize;
    let mut contained = Vec::new();
    for subject in &MATRIX {
        let mine = pinned_capabilities(subject);
        for other in &MATRIX {
            if other.family == subject.family {
                continue;
            }
            pairs += 1;
            let theirs = pinned_capabilities(other);
            if !theirs.is_empty() && theirs.is_subset(&mine) {
                contained.push((subject.family.label(), other.family.label()));
            }
        }
    }
    assert_eq!(
        pairs,
        MATRIX.len() * (MATRIX.len() - 1),
        "the matrix is not the full ordered-pair set"
    );
    contained.sort();
    let observed: Vec<(&str, &str)> = contained
        .iter()
        .map(|(subject, object)| (subject.as_str(), object.as_str()))
        .collect();
    assert_eq!(
        observed, CONTAINED_PAIRS,
        "the ordered pairs that carry NO denial moved: a widening that swallows \
         another family's surface whole turns that pair's red into silence"
    );
}

/// The ordered pairs `(subject, object)` where the OBJECT's whole measured
/// surface is contained in the SUBJECT's, so the pair owes no denial.
///
/// Not a defect — it is what the measured surfaces are — but it is the set of
/// pairs the matrix cannot speak for, so it is spelled out rather than left to
/// be counted. Pairs naming one of the two MEASURED-EMPTY families as the object
/// are excluded: those are asserted separately, by name, above.
const CONTAINED_PAIRS: [(&str, &str); 6] = [
    ("app", "http-admitter"),
    ("app", "retention"),
    ("effect-writer", "dispatch-reader"),
    ("executor-platform", "dispatch-reader"),
    ("executor-platform", "http-admitter"),
    ("management-admitter", "http-admitter"),
];

// ---------------------------------------------------------------------------
// THE EFFECT-WRITER ARMS — four of them, three previously unguarded
// ---------------------------------------------------------------------------

/// The four `TO wamn_effect_writer` arms, EXCESS AND MISSING BOTH FAILING.
///
/// # Why this is a row-count arm and not a privilege arm
///
/// `wamn-0h0g.22.32` landed four per-relation `AS PERMISSIVE … TO
/// wamn_effect_writer USING (true)` arms. Measured when they landed: dropping
/// `runs_effect_writer`, or the dispatches or outcomes arm, left BOTH
/// `deploy_sql_authority` and `run_plane_live` at full green while the minted
/// writer's read of `wamn_run.runs` went to ZERO ROWS. THREE OF THE FOUR WERE
/// COMPLETELY UNGUARDED. `has_table_privilege` cannot see it: the grant is
/// untouched and only the policy is gone, so the read is refused at zero rows
/// with no error.
///
/// # What the claim is, and what it is NOT
///
/// The arms are `USING (true)`, so the claim is the ARM'S REACH, never
/// cross-tenant confinement: one project-environment database serves exactly one
/// tenant, and the arm restores exactly the reach the platform membership gave
/// before `wamn-0h0g.22.32` demoted the writer out of `wamn_platform`. The
/// strictly-tighter clause was WITHDRAWN and is deliberately not asserted here.
#[test]
fn the_effect_writer_arm_reaches_exactly_its_four_run_plane_ledgers() {
    let Some(fixture) = armed("the_effect_writer_arm_reaches_exactly_its_four_run_plane_ledgers")
    else {
        return;
    };
    let writer = WorkloadRoleFamily::EffectWriter;

    // --- EXCESS: no FIFTH relation carries an arm naming the writer ---------
    assert_eq!(
        rows(
            &fixture.db_url,
            &format!(
                "SELECT n.nspname || '.' || c.relname \
                   FROM pg_catalog.pg_policy p \
                   JOIN pg_catalog.pg_class c ON c.oid = p.polrelid \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                  WHERE p.polroles @> ARRAY['{role}'::regrole::oid] \
                  ORDER BY (n.nspname || '.' || c.relname) COLLATE \"C\"",
                role = writer.acl_role(),
            ),
        ),
        vec![
            "wamn_run.effect_attempt_dispatches",
            "wamn_run.effect_attempt_outcomes",
            "wamn_run.effect_attempts",
            "wamn_run.runs",
        ],
        "the set of relations carrying a policy arm named for the effect-writer \
         family is not exactly the four run-plane ledgers"
    );

    // --- MISSING: each of the four is actually READ, as the minted login -----
    let login = generation(writer);
    let as_writer = login_url(&fixture.admin, &login);
    for ledger in [
        "wamn_run.runs",
        "wamn_run.effect_attempts",
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
    ] {
        assert_eq!(
            query(&as_writer, &format!("SELECT count(*) FROM {ledger}")),
            "2",
            "the effect-writer family reads no rows from {ledger}: its arm is \
             missing, and under FORCE RLS that is a SILENT lockout, not an error"
        );
    }

    // --- THE ARM BUYS NOTHING WITHOUT A GRANT -------------------------------
    // A relation the writer holds no grant on is refused LOUDLY, which is what
    // separates "refused" from "matched nothing" everywhere above.
    assert_eq!(
        sqlstate(
            &as_writer,
            "SELECT count(*) FROM wamn_run.environment_policies;\n"
        )
        .as_deref(),
        Some("42501"),
        "the effect-writer family reached a relation it holds no grant on"
    );

    // --- THE CONTROL THAT LEGITIMATELY READS ZERO ---------------------------
    // Same table grant, member of nothing: zero rows and NO error. Without this
    // arm the four counts above could not be told from a lucky fixture.
    let as_outsider = login_url(&fixture.admin, OUTSIDER);
    assert_eq!(
        query(
            &as_outsider,
            "SELECT count(*) FROM wamn_run.effect_attempts"
        ),
        "0",
        "a principal in NEITHER group, holding the same table grant, read rows: \
         the tenant floor is not default-denying"
    );
}

// ---------------------------------------------------------------------------
// THE PLATFORM-GRAIN ARMS
// ---------------------------------------------------------------------------

/// A PLATFORM FAMILY WITH GRANTS AND NO TENANT CONTEXT READS EXACTLY ITS GRANTS
/// (`wamn-0h0g.22.17`).
///
/// The tenant floor is narrowed `TO wamn_app`, and PostgreSQL DEFAULT-DENIES
/// when RLS is on and no policy matches — so the narrowing does not EXEMPT a
/// platform principal, it LOCKS IT OUT at zero rows with no exception. What
/// admits it is one permissive `TO wamn_platform` arm per relation, and what
/// still BOUNDS it is its own table grants. Both halves are read back here: rows
/// where a grant exists, `42501` where it does not.
#[test]
fn a_platform_family_without_tenant_context_reads_exactly_what_its_grants_say() {
    let Some(fixture) =
        armed("a_platform_family_without_tenant_context_reads_exactly_what_its_grants_say")
    else {
        return;
    };
    let executor = WorkloadRoleFamily::ExecutorPlatform;
    assert!(
        executor.is_platform_grain(),
        "this arm's subject must be a platform-grain family"
    );
    let login = generation(executor);

    // The two-hop chain (generation -> stable ACL role -> wamn_platform) is
    // walked by the PER-EDGE inherit_option, and a bare GRANT onto a NOINHERIT
    // stable role lands it false and reads zero rows in silence.
    assert_eq!(
        query(
            &fixture.db_url,
            &format!("SELECT pg_has_role('{login}', '{PLATFORM_GROUP_ROLE}', 'USAGE')::text"),
        ),
        "true",
        "the executor-platform generation does not effectively hold \
         {PLATFORM_GROUP_ROLE}: every platform read would be zero rows"
    );

    let as_executor = login_url(&fixture.admin, &login);
    // It has NO tenant context and structurally cannot acquire one: the session
    // derivation is the guest's, and a platform family holds neither `USAGE` on
    // `wamn_authority` nor `EXECUTE` on it. So BOTH tenants' rows are the
    // correct answer below — the confinement is the grant set, not a predicate.
    assert_eq!(
        sqlstate(
            &as_executor,
            "SELECT wamn_authority.current_tenant_key();\n"
        )
        .as_deref(),
        Some("42501"),
        "a platform family reached the guest session derivation; if it can \
         derive a tenant key, this arm is measuring a guest"
    );
    for (relation, expected) in [("wamn_run.runs", "2"), ("wamn_run.effect_attempts", "2")] {
        assert_eq!(
            query(&as_executor, &format!("SELECT count(*) FROM {relation}")),
            expected,
            "the executor-platform family reads {expected} rows from {relation} \
             on its grants; a different count means the platform arm admits it \
             partially or not at all"
        );
    }
    // …and NOTHING MORE. These two are the effect writer's and the guest's.
    for relation in [
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
    ] {
        assert_eq!(
            sqlstate(&as_executor, &format!("SELECT count(*) FROM {relation};\n")).as_deref(),
            Some("42501"),
            "the executor-platform family reached {relation}, which its grant \
             set does not name: the permissive platform arm is USING (true), so \
             a grant here would be unbounded reach"
        );
    }
}

/// THE TENANT-SCOPED PLATFORM MEMBER (`wamn-0h0g.22.25`).
///
/// # A premise of that ruling no longer holds
///
/// The ruling named TWO tenant-scoped `wamn_platform` members, `EffectWriter`
/// and `Retention`. `wamn-0h0g.22.32` then DEMOTED the writer out of the group —
/// its stable role is under a shape guard that refuses ANY `pg_auth_members` row
/// — so `Retention` is the only subject the arm still has. That is asserted here
/// rather than remembered, so the day a second one appears this arm reds.
#[test]
fn the_tenant_scoped_platform_member_reads_nothing_outside_its_grants() {
    let Some(fixture) = armed("the_tenant_scoped_platform_member_reads_nothing_outside_its_grants")
    else {
        return;
    };
    let tenant_scoped_members: Vec<WorkloadRoleFamily> = MATRIX
        .iter()
        .map(|reach| reach.family)
        .filter(|family| {
            matches!(family.scope_kind(), WorkloadRoleScopeKind::Tenant)
                && family.is_platform_grain()
        })
        .collect();
    assert_eq!(
        tenant_scoped_members,
        vec![WorkloadRoleFamily::Retention],
        "the set of TENANT-SCOPED platform members moved; wamn-0h0g.22.25 was \
         ruled for exactly the families in this set"
    );

    let login = generation(WorkloadRoleFamily::Retention);
    let as_retention = login_url(&fixture.admin, &login);
    // Its grant set names ONE relation, and the shared arm is USING (true), so
    // the honest claim is: every row of that relation, and 42501 everywhere
    // else. The tenant predicate it keeps in its own statements is what
    // re-narrows it, and that is not a database-enforced boundary.
    assert_eq!(
        query(&as_retention, "SELECT count(*) FROM wamn_run.runs"),
        "2",
        "the retention family cannot read the relation its grant set names"
    );
    for relation in [
        "wamn_run.effect_attempts",
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
        "wamn_run.environment_policies",
        "wamn_run.run_queue",
        "catalog.wirings",
    ] {
        assert_eq!(
            sqlstate(
                &as_retention,
                &format!("SELECT count(*) FROM {relation};\n")
            )
            .as_deref(),
            Some("42501"),
            "the retention family reached {relation} while holding platform \
             membership: the shared arm must buy it nothing beyond its grants"
        );
    }
}

/// THE GUEST IS THE ONE FAMILY THE FLOOR CONFINES BY ROW.
///
/// Its ACL says `SELECT` on `wamn_run.runs` and the server agrees; what the ACL
/// cannot say is that it sees ONE tenant's row out of two. This is the arm that
/// separates an isolated credential from a blind one for the family whose
/// confinement is RLS-shaped rather than grant-shaped.
#[test]
fn the_guest_family_reads_its_own_tenant_and_only_its_own() {
    let Some(fixture) = armed("the_guest_family_reads_its_own_tenant_and_only_its_own") else {
        return;
    };
    let login = generation(WorkloadRoleFamily::App);
    let as_guest = login_url(&fixture.admin, &login);
    assert_eq!(
        query(
            &as_guest,
            &format!(
                "SELECT count(*)::text || ' ' || \
                        count(*) FILTER (WHERE tenant_id = '{TENANT_B}')::text \
                   FROM wamn_run.runs"
            ),
        ),
        "1 0",
        "the guest generation does not read exactly its own tenant's row: a \
         second row is a CROSS-TENANT READ and zero rows is a silent lockout"
    );
    // A settable claim must not move it. The floor keys on `current_user`, and
    // this is what proves the retired `app.tenant` path is inert for the guest.
    assert_eq!(
        query(
            &as_guest,
            &format!(
                "BEGIN; SET LOCAL app.tenant = '{TENANT_B}'; \
                 SELECT count(*) FROM wamn_run.runs; COMMIT"
            ),
        ),
        "1",
        "a session-settable claim moved the guest's row visibility"
    );
}

// ---------------------------------------------------------------------------
// INDEX COVERAGE — the EXPRESSION, read back
// ---------------------------------------------------------------------------

/// The expression every governed relation's index must carry, verbatim.
const TENANT_KEY_INDEX_EXPRESSION: &str = "wamn_authority.tenant_key(tenant_id)";

/// EVERY RELATION CARRYING THE GUEST PREDICATE HAS ITS EXPRESSION INDEX, AND
/// THE EXPRESSION IS READ BACK.
///
/// # Presence is not coverage
///
/// `CREATE INDEX IF NOT EXISTS` is satisfied by an index that already exists
/// UNDER THE WRONG EXPRESSION — over a different column, or over a different
/// function — and that index leaves every guest read non-sargable exactly as if
/// no index existed. So this arm compares `pg_get_expr(indexprs, indrelid)` to
/// the expression the predicate calls, and does it in BOTH directions: a
/// governed relation with no matching index, and an index whose expression
/// matches on a relation that carries no governed predicate, are both reds. The
/// two-way form is also what removes the need for a second copy of the governed
/// COUNT: the sets are compared to each other, not to a remembered number.
#[test]
fn every_governed_relation_carries_the_tenant_key_expression_index() {
    let Some(fixture) = armed("every_governed_relation_carries_the_tenant_key_expression_index")
    else {
        return;
    };
    let governed = rows(
        &fixture.db_url,
        "SELECT n.nspname || '.' || c.relname \
           FROM pg_catalog.pg_policy p \
           JOIN pg_catalog.pg_class c ON c.oid = p.polrelid \
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
          WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%' \
          ORDER BY (n.nspname || '.' || c.relname) COLLATE \"C\"",
    );
    let indexed = rows(
        &fixture.db_url,
        &format!(
            "SELECT n.nspname || '.' || c.relname \
               FROM pg_catalog.pg_index i \
               JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
              WHERE i.indexprs IS NOT NULL \
                AND pg_get_expr(i.indexprs, i.indrelid) = '{TENANT_KEY_INDEX_EXPRESSION}' \
              ORDER BY (n.nspname || '.' || c.relname) COLLATE \"C\""
        ),
    );
    assert_eq!(
        governed, indexed,
        "the relations carrying the guest tenant predicate and the relations \
         carrying an index on {TENANT_KEY_INDEX_EXPRESSION} are not the same \
         set: a governed relation missing from the second list makes every guest \
         read non-sargable, and one missing from the first is an index nothing \
         governs"
    );
    // A floor, so an empty schema cannot satisfy the equality above trivially.
    // The five run-plane relations are named because they are the ones whose
    // policies the effect-writer and platform arms share.
    for relation in [
        "wamn_run.runs",
        "wamn_run.effect_attempts",
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
        "wamn_run.environment_policies",
    ] {
        assert!(
            governed.iter().any(|row| row == relation),
            "{relation} carries no governed predicate at all"
        );
    }
    // The index is only sargable if the derivation is IMMUTABLE, so the flag is
    // read from the catalog rather than assumed from the index's existence.
    assert_eq!(
        query(
            &fixture.db_url,
            "SELECT p.provolatile::text FROM pg_catalog.pg_proc p \
               JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_authority' AND p.proname = 'tenant_key'",
        ),
        "i",
        "the derivation lost IMMUTABLE; the expression indexes above are not \
         rebuildable and the predicate is not sargable"
    );
}

// ---------------------------------------------------------------------------
// THE FROZEN FUNCTION PROBE
// ---------------------------------------------------------------------------

/// `sha256(pg_get_functiondef(...))` for the two authority derivations, PINNED.
///
/// `tenant_key`'s body carries the database name and its octet length, so its
/// digest is per-database; this is the value for the `wamn` database
/// `deploy/sql/postgres-init.sql` creates, which is the database this gate
/// builds. `current_tenant_key` carries no scope and its digest is absolute.
///
/// Measured on PostgreSQL 18.6. `pg_get_functiondef` is a RENDERING, so a server
/// that reformats it moves these digests without anything about the derivation
/// changing; the assertion prints the observed definition so that case is
/// diagnosable rather than merely red.
const FROZEN_DERIVATION_DIGESTS: [&str; 2] = [
    "current_tenant_key 448719229148c4c5051b50ecbe55d70d8ff22bbcf652e3f12387296dd3fa1eb8",
    "tenant_key 4390e3ef73c0cfffec72585be90345a77c9436118202f4a1d23da77156a150e8",
];

/// THE DERIVATIONS ARE FROZEN BY DIGEST.
///
/// An attacker who can redefine `wamn_authority.tenant_key` owns EVERY governed
/// predicate at once, and `current_tenant_key` is the half worth owning more:
/// redefining it to return a chosen key unlocks every tenant without touching a
/// single policy. Neither can be rewritten without moving a digest here.
#[test]
fn the_authority_derivations_match_their_pinned_definition_digest() {
    let Some(fixture) = armed("the_authority_derivations_match_their_pinned_definition_digest")
    else {
        return;
    };
    let observed = rows(
        &fixture.db_url,
        "SELECT p.proname || ' ' \
                || encode(sha256(convert_to(pg_get_functiondef(p.oid), 'UTF8')), 'hex') \
           FROM pg_catalog.pg_proc p \
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'wamn_authority' \
          ORDER BY p.proname COLLATE \"C\"",
    );
    if observed != FROZEN_DERIVATION_DIGESTS {
        let definitions = query(
            &fixture.db_url,
            "SELECT string_agg(pg_get_functiondef(p.oid), E'\\n---\\n' ORDER BY p.proname) \
               FROM pg_catalog.pg_proc p \
               JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_authority'",
        );
        panic!(
            "the installed authority derivations do not match their pinned \
             digests.\nobserved: {observed:?}\npinned:   \
             {FROZEN_DERIVATION_DIGESTS:?}\n--- installed ---\n{definitions}"
        );
    }
}

/// The two run-plane guards are PUBLIC EXECUTE today, which is why the routine
/// half of the matrix reads `aclexplode` and not `has_function_privilege`.
///
/// Recorded as a measured fact rather than assumed, so that revoking PUBLIC —
/// which would make `has_function_privilege` discriminating and is a change this
/// gate is not the owner of — is a deliberate edit here.
#[test]
fn the_run_plane_guards_are_still_public_execute() {
    let Some(fixture) = armed("the_run_plane_guards_are_still_public_execute") else {
        return;
    };
    assert_eq!(
        query(
            &fixture.db_url,
            "SELECT has_function_privilege('public', \
               'wamn_run.require_executor_platform_authority()', 'EXECUTE')::text || ' ' || \
             has_function_privilege('public', \
               'wamn_run.require_management_admission_authority()', 'EXECUTE')::text || ' ' || \
             has_function_privilege('public', \
               'wamn_authority.tenant_key(text)', 'EXECUTE')::text || ' ' || \
             has_function_privilege('public', \
               'wamn_authority.current_tenant_key()', 'EXECUTE')::text",
        ),
        "true true false false",
        "PUBLIC's EXECUTE on the run-plane guards or on the authority \
         derivations moved; the routine arms of this matrix are built on the \
         first pair being PUBLIC and the second pair not being"
    );
}

// ---------------------------------------------------------------------------
// THE PLATFORM GROUP'S MEMBER SET, AND THE ONE RATIFIED EXCEPTION
// ---------------------------------------------------------------------------

/// The stable ACL roles [`WorkloadRoleFamily::is_platform_grain`] yields, PINNED
/// AS A VALUE.
///
/// NOT derived here, deliberately. `sql::platform_group_membership_sql` grants
/// the edge by calling that very function, so an arm comparing the server to the
/// function would move BOTH SIDES under a mutant and stay true — the tautology
/// this branch has paid for before. The pin is the second document, and
/// admitting or demoting a family costs one deliberate edit here.
///
/// Sorted, because the arm compares against a sorted list.
const PLATFORM_GRAIN_ACL_ROLES: [&str; 7] = [
    "wamn_dispatch_reader",
    "wamn_event_materializer",
    "wamn_executor_platform",
    "wamn_http_admitter",
    "wamn_management_admitter",
    "wamn_run_retention",
    "wamn_service_reader",
];

/// THE MEMBER SET OF `wamn_platform`, FROM `pg_auth_members`.
///
/// It is exactly the seven derived workload families. `wamn_scenario_author`
/// has no exception: it has no production credential or project-plane read.
#[test]
fn the_platform_group_members_are_exactly_the_derived_families() {
    let Some(fixture) = armed("the_platform_group_members_are_exactly_the_derived_families") else {
        return;
    };

    // The pin and the derivation are checked against EACH OTHER first, so a red
    // below names which of the two moved rather than only that they disagree.
    let derived: Vec<&str> = {
        let mut roles: Vec<&str> = WorkloadRoleFamily::ALL
            .iter()
            .filter(|family| family.is_platform_grain())
            .map(|family| family.acl_role())
            .collect();
        roles.sort_unstable();
        roles
    };
    assert_eq!(
        derived, PLATFORM_GRAIN_ACL_ROLES,
        "is_platform_grain no longer yields the pinned family set: a family was \
         admitted or demoted, and the membership arm below cannot tell which \
         until this pin is moved deliberately"
    );

    let mut expected: Vec<String> = PLATFORM_GRAIN_ACL_ROLES
        .iter()
        .map(|role| (*role).to_owned())
        .collect();
    expected.sort();
    let observed = rows(
        &fixture.db_url,
        &format!(
            "SELECT row FROM ( \
               SELECT member.rolname::text AS row \
                 FROM pg_catalog.pg_auth_members edge \
                 JOIN pg_catalog.pg_roles group_role ON group_role.oid = edge.roleid \
                 JOIN pg_catalog.pg_roles member ON member.oid = edge.member \
                WHERE group_role.rolname = '{PLATFORM_GROUP_ROLE}' \
             ) q ORDER BY row COLLATE \"C\"",
        ),
    );
    assert_eq!(
        observed, expected,
        "the {PLATFORM_GROUP_ROLE} member set is not exactly the seven derived \
         workload families; no host-only exception is admitted"
    );

    // Every edge's OPTIONS, not merely its existence. PostgreSQL 16+ takes a new
    // membership's INHERIT default from the MEMBER's `rolinherit`, and every
    // stable ACL role is minted NOINHERIT — so a bare `GRANT` lands
    // `inherit_option = false`, RLS role matching skips the edge, and the member
    // reads zero rows in silence while `pg_auth_members` still shows a row.
    assert_eq!(
        query(
            &fixture.db_url,
            &format!(
                "SELECT coalesce(bool_and(edge.inherit_option \
                                          AND NOT edge.admin_option \
                                          AND NOT edge.set_option), false)::text \
                   FROM pg_catalog.pg_auth_members edge \
                   JOIN pg_catalog.pg_roles group_role ON group_role.oid = edge.roleid \
                  WHERE group_role.rolname = '{PLATFORM_GROUP_ROLE}'"
            ),
        ),
        "true",
        "a {PLATFORM_GROUP_ROLE} edge is not INHERIT TRUE, ADMIN FALSE, SET \
         FALSE: the edge exists and confers nothing, which is a silent lockout \
         with a row in the catalog to prove it should not be"
    );
}

/// The reserved scenario-author role has neither a platform-policy edge nor a
/// table read. The synthetic inheriting login proves this as a real SQL refusal,
/// not merely as an ACL catalog observation.
#[test]
fn the_scenario_author_has_no_platform_membership_or_project_reads() {
    let Some(fixture) = armed("the_scenario_author_has_no_platform_membership_or_project_reads")
    else {
        return;
    };

    assert_eq!(
        query(
            &fixture.db_url,
            &format!(
                "SELECT (NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                         AND NOT rolcreaterole AND NOT rolinherit \
                         AND NOT rolreplication AND NOT rolbypassrls)::text \
                   FROM pg_catalog.pg_roles \
                  WHERE rolname = '{SCENARIO_AUTHOR_ROLE}'"
            ),
        ),
        "true",
        "{SCENARIO_AUTHOR_ROLE} is not the stable NOLOGIN role"
    );
    assert_eq!(
        query(
            &fixture.db_url,
            &format!(
                "SELECT pg_has_role('{SCENARIO_AUTHOR_ROLE}', \
                   '{PLATFORM_GROUP_ROLE}', 'MEMBER')::text"
            ),
        ),
        "false",
        "{SCENARIO_AUTHOR_ROLE} acquired the forbidden ninth platform edge"
    );

    let exposed = rows(
        &fixture.db_url,
        &format!(
            "SELECT row FROM ( \
               SELECT n.nspname || '.' || c.relname AS row \
                 FROM pg_catalog.pg_policy p \
                 JOIN pg_catalog.pg_class c ON c.oid = p.polrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%' \
                  AND has_table_privilege('{SCENARIO_AUTHOR_ROLE}', c.oid, 'SELECT') \
             ) q ORDER BY row COLLATE \"C\"",
        ),
    );
    assert!(
        exposed.is_empty(),
        "{SCENARIO_AUTHOR_ROLE} retains governed project reads: {exposed:?}"
    );

    let as_author = login_url(&fixture.admin, SCENARIO_AUTHOR_PROBE);
    for relation in ["wamn_run.environment_policies", "wamn_run.runs"] {
        assert_eq!(
            query(
                &fixture.db_url,
                &format!(
                    "SELECT has_table_privilege('{SCENARIO_AUTHOR_PROBE}', \
                       '{relation}', 'SELECT')::text"
                ),
            ),
            "false",
            "the synthetic author login inherits SELECT on {relation}"
        );
        assert_eq!(
            sqlstate(&as_author, &format!("SELECT count(*) FROM {relation};\n")).as_deref(),
            Some("42501"),
            "the synthetic author login did not receive the server's permission denial on {relation}"
        );
    }
}
