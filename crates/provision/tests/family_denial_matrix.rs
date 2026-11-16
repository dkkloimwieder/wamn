//! Database authority through real reads and refused writes.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use url::Url;

use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{
    PLATFORM_GROUP_ROLE, WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType,
    workload_login,
};
use wamn_control_provision::{CONTROL_PORTABLE_STORE_SQL, SYSTEM_SCHEMA_SQL};

const POSTGRES_INIT: &str = include_str!("../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const RUN_STATE: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../deploy/sql/app-schema.sql");

/// The database `deploy/sql/postgres-init.sql` creates. Also the target every
/// generation login's digest is taken over, so it cannot be chosen freely.
const DATABASE: &str = "wamn";

/// The control database of this acceptance test. The control-plane families reach it
/// and nothing else.
const CONTROL_DATABASE: &str = "wamn_matrix_control";

const ORG: &str = "acme";
const PROJECT: &str = "billing";
const ENVIRONMENT: &str = "dev";

/// The project every project-targeted generation in this acceptance test is minted for, and
/// the project the guest may read. `PROJECT_B` is the neighbour it may not.
const PROJECT_ENV_KEY_A: &str = "acme--dev--billing";
const PROJECT_ENV_KEY_B: &str = "acme--dev--other";

/// Throwaway password for the probe logins. Not a credential of record: the
/// condition builds and drops these roles inside a disposable container.
const PROBE_PASSWORD: &str = "w68-matrix-probe";

// ---------------------------------------------------------------------------
// psql
// ---------------------------------------------------------------------------

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal is asserted on `42501` rather than on an error string.
fn psql(url: &str, script: &str) -> (bool, String, String) {
    let script = script.replace(
        "\\ir record-history.sql",
        include_str!("../../../deploy/sql/record-history.sql"),
    );
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

fn bound_query(url: &str, statement: &str) -> String {
    query(
        url,
        &format!(
            "BEGIN; {} {statement}; COMMIT",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            )
        ),
    )
}

fn rows(url: &str, sql: &str) -> Vec<String> {
    query(url, sql)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}


/// The admin URL with its userinfo replaced by one role's login, pointed at the
/// project database.
fn login_url(admin_url: &str, role: &wamn_identity_protocol::LoginName) -> String {
    let mut url = Url::parse(admin_url).expect("the test server URL is a URL");
    url.set_username(role.as_str()).expect("set the probe role");
    url.set_password(Some(PROBE_PASSWORD))
        .expect("set password");
    url.set_path(DATABASE);
    url.into()
}

/// The database a family's credential reaches.
fn database_of(family: WorkloadRoleFamily) -> &'static str {
    match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey | WorkloadRoleIdentityType::ProjectEnvironment => {
            DATABASE
        }
        WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org => CONTROL_DATABASE,
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    _server: wamn_test_postgres::OwnedPostgres,
    admin: String,
    db_url: String,
    control_url: String,
}

/// The deterministic generation name the real mint would issue for one family.
fn generation(family: WorkloadRoleFamily) -> wamn_identity_protocol::LoginName {
    let target = match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &PROJECT_ENV_KEY_A.parse().unwrap(),
            database: DATABASE,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org: &ORG.parse().unwrap(),
            project: &PROJECT.parse().unwrap(),
            environment: &ENVIRONMENT.parse().unwrap(),
            database: DATABASE,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: &ORG.parse().unwrap(),
            project: &PROJECT.parse().unwrap(),
            environment: &ENVIRONMENT.parse().unwrap(),
            database: CONTROL_DATABASE,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: &ORG.parse().unwrap(),
            database: CONTROL_DATABASE,
        },
    };
    workload_login(family, target)
        .unwrap_or_else(|error| panic!("mint a {family:?} generation: {error}"))
}

/// Roles this acceptance test creates, drops or re-mints. Roles are CLUSTER-WIDE.
// Package migration credentials require their package schema, outside this fixture.
fn families() -> impl Iterator<Item = WorkloadRoleFamily> {
    WorkloadRoleFamily::ALL
        .into_iter()
        .filter(|family| *family != WorkloadRoleFamily::Migration)
}

fn managed_roles() -> Vec<String> {
    let mut roles: Vec<String> = families()
        .flat_map(|family| [family.acl_role().to_owned(), generation(family).to_string()])
        .collect();
    // The probe comes BEFORE the group it inherits: a role still named by a
    // `pg_auth_members` row cannot be dropped.
    roles.push(PLATFORM_GROUP_ROLE.to_owned());
    roles
}

/// Rebuild the whole world from scratch.
///
/// HERMETIC ON PURPOSE, and it must FAIL on a dirty cluster rather than repair
/// one quietly: `postgres-init.sql` carries bare `CREATE ROLE`s, so a leftover
/// HEALTHY role would satisfy an `IF NOT EXISTS` elsewhere and MASK a mutated
/// builder. `DROP OWNED BY` precedes `DROP ROLE`, inside an existence test, and
/// the DATABASE goes first — `DROP OWNED BY` reaches only the current database,
/// and a role still named by a `relacl` cannot be dropped.
fn reset(admin: &str) {
    apply(
        admin,
        "drop the project and control databases",
        &format!(
            "DROP DATABASE IF EXISTS \"{DATABASE}\";\n\
             DROP DATABASE IF EXISTS \"{CONTROL_DATABASE}\";\n"
        ),
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
        "close the stock PUBLIC connect minimum",
        "REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n\
         REVOKE CONNECT ON DATABASE template1 FROM PUBLIC;\n",
    );
}

/// Two projects' worth of rows in every relation an RLS-reach arm reads back.
///
/// Two, not one: an arm that reads its own project cannot tell confinement from
/// an empty table, and the platform arm's whole assertion is that it reads BOTH.
const SEED: &str = "\
DO $seed$ DECLARE t text; BEGIN
FOREACH t IN ARRAY ARRAY['acme--dev--billing','acme--dev--other'] LOOP
  INSERT INTO catalog.packages ( project_env_key, package_id, package_version, artifact_digest) VALUES ( t, 'widgets', '1.0.0', 'sha256:'||repeat('f',64));
  INSERT INTO catalog.releases
    (project_env_key, manifest_digest, canonical_bytes)
    VALUES (t, 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', '{}');
  INSERT INTO wamn_run.runs (project_env_key, run_id, package_id, environment, status, trigger_source, wiring_id, wiring_version, wiring_hash, input_json, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json, manifest_digest) VALUES (t, 'r1', 'widgets', 'dev', 'dispatched', 'manual', 'w', 1, 'sha256:'||repeat('c',64), '{\"a\":1}', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{}', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a');
END LOOP;
END $seed$;
";

/// A package-shaped schema with two logged relations and their history tables.
///
/// apply-package creates this shape: the history tables come from the platform
/// function, and each log trigger carries its retention. `entry` keeps its
/// entries for 30 days, and `ledger` keeps them without limit.
const RETENTION_FIXTURE: &str = "\
CREATE SCHEMA retention_fixture;
CREATE TABLE retention_fixture.entry (id uuid CONSTRAINT entry_id_pkey PRIMARY KEY);
CREATE TABLE retention_fixture.ledger (id uuid CONSTRAINT ledger_id_pkey PRIMARY KEY);
SELECT wamn_history.create_history_table('retention_fixture', 'entry', false);
SELECT wamn_history.create_history_table('retention_fixture', 'ledger', false);
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON retention_fixture.entry
  FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('P30D');
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON retention_fixture.ledger
  FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');
";

static FIXTURE: OnceLock<Fixture> = OnceLock::new();

/// Build the world once per process on its own server: the real artifacts, the
/// real convergent grant batches, and one minted generation login per family.
fn build() -> Fixture {
    let server = wamn_test_postgres::start(&[]).expect("start the denial matrix PostgreSQL server");
    let admin = server
        .database("postgres")
        .expect("the denial matrix server has its postgres database")
        .url()
        .to_owned();
    reset(&admin);
    apply(&admin, "apply postgres-init.sql", POSTGRES_INIT);
    // The admin URL keeps its own userinfo and moves to the project database;
    // `login_url` is for the probe logins.
    let mut admin_db = Url::parse(&admin).expect("the test server URL is a URL");
    admin_db.set_path(DATABASE);
    let db_url = admin_db.to_string();
    for artifact in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, "apply a schema artifact", artifact);
    }
    apply(
        &db_url,
        "seed two projects",
        &format!(
            "BEGIN; {} {SEED} COMMIT;",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            )
        ),
    );
    apply(
        &db_url,
        "create the logged fixture relations",
        RETENTION_FIXTURE,
    );
    for family in families().filter(|family| database_of(*family) == DATABASE) {
        mint(&db_url, family);
    }
    let control_url = build_control_plane(&admin);

    Fixture {
        _server: server,
        admin,
        db_url,
        control_url,
    }
}

/// The control database as a fresh control bootstrap leaves it, and one minted
/// generation login per control family. Returns its admin URL.
///
/// `wamn_system` owns the database and applies the control store, as the
/// control bootstrap does.
fn build_control_plane(admin: &str) -> String {
    apply(
        admin,
        "create the control owner role and the control database",
        &format!(
            "DO $system_role$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $system_role$;\n\
             {db_owner}\n\
             CREATE DATABASE \"{CONTROL_DATABASE}\" OWNER wamn_system;\n\
             REVOKE CONNECT, TEMPORARY ON DATABASE \"{CONTROL_DATABASE}\" FROM PUBLIC;\n",
            db_owner = sql::ensure_db_owner_role_sql(),
        ),
    );
    let mut control = Url::parse(admin).expect("the test server URL is a URL");
    control.set_path(CONTROL_DATABASE);
    let control_url = control.to_string();
    apply(
        &control_url,
        "install the control store as its owner",
        &format!(
            "SET ROLE wamn_system;\n{SYSTEM_SCHEMA_SQL}\n{CONTROL_PORTABLE_STORE_SQL}\nRESET ROLE;\n"
        ),
    );
    for family in families().filter(|family| database_of(*family) == CONTROL_DATABASE) {
        mint(&control_url, family);
    }
    control_url
}

/// Prepare one family's stable surface and one generation login.
///
/// The guest now takes the same generic prepare path as every other family.
/// `postgres-init.sql` and `ensure_workload_acl_role_sql(App)` both converge
/// `wamn_app` to `NOLOGIN PASSWORD NULL NOINHERIT`, so preparing a generation
/// no longer repairs a contradictory install-time LOGIN as a side effect. This
/// condition therefore exercises the production App builder directly.
fn mint(db_url: &str, family: WorkloadRoleFamily) {
    let login = generation(family);
    apply(
        db_url,
        "prepare a workload generation",
        &wamn_control_provision::test_database::login_sql(
            family,
            database_of(family),
            &login,
            PROBE_PASSWORD,
        ),
    );
}

/// The world of this process, built on first use.
fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(build)
}

// ---------------------------------------------------------------------------
// THE PLATFORM-GRAIN ARMS
// ---------------------------------------------------------------------------

/// THE GUEST IS THE ONE FAMILY THE MINIMUM CONFINES BY ROW.
///
/// Its ACL says `SELECT` on `wamn_run.runs` and the server agrees; what the ACL
/// cannot say is that it sees ONE project's row out of two. This is the arm that
/// separates an isolated credential from a blind one for the family whose
/// confinement is RLS-shaped rather than grant-shaped.
#[test]
#[ignore = "requires: postgres, bin:psql"]
fn the_guest_family_reads_its_own_project_env_key_and_only_its_own() {
    let fixture = fixture();
    let login = generation(WorkloadRoleFamily::App);
    let as_guest = login_url(&fixture.admin, &login);
    assert_eq!(
        bound_query(
            &as_guest,
            &format!(
                "SELECT count(*)::text || ' ' || \
                        count(*) FILTER (WHERE project_env_key = '{PROJECT_ENV_KEY_B}')::text \
                   FROM wamn_run.runs"
            ),
        )
        .split_whitespace()
        .map(|value| value.parse::<u64>().unwrap())
        .collect::<Vec<_>>(),
        vec![1, 0],
        "the guest generation does not read exactly its own project's row: a \
         second row is a CROSS-PROJECT READ and zero rows is a silent lockout"
    );
}

// ---------------------------------------------------------------------------
// THE PLATFORM GROUP'S MEMBER SET, AND THE ONE RATIFIED EXCEPTION
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires: postgres, bin:psql"]
fn the_platform_group_members_are_exactly_the_derived_families() {
    let fixture = fixture();

    let mut expected: Vec<String> = families()
        .filter(|family| family.is_platform_grain())
        .map(|family| family.acl_role().to_owned())
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
        "the {PLATFORM_GROUP_ROLE} member set is not exactly the derived \
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
        )
        .parse::<bool>()
        .unwrap(),
        true,
        "a {PLATFORM_GROUP_ROLE} edge is not INHERIT TRUE, ADMIN FALSE, SET \
         FALSE: the edge exists and confers nothing, which is a silent lockout \
         with a row in the catalog that requires its absence"
    );
}

// ---------------------------------------------------------------------------
// Unconditional policies of the control database
// ---------------------------------------------------------------------------

/// Installed control policies must not publish a constant true predicate.
#[test]
#[ignore = "requires: postgres, bin:psql"]
fn no_control_policy_passes_every_row() {
    let fixture = fixture();
    let policies: Vec<[Option<String>; 2]> = serde_json::from_str(&query(
        &fixture.control_url,
        "SELECT coalesce(jsonb_agg(jsonb_build_array(pg_get_expr(polqual, polrelid), pg_get_expr(polwithcheck, polrelid))), '[]'::jsonb)::text FROM pg_catalog.pg_policy",
    )).unwrap();
    for expression in policies.into_iter().flatten().flatten() {
        let parsed = pg_query::parse(&format!("SELECT 1 WHERE {expression}")).unwrap();
        let pg_query::NodeEnum::SelectStmt(statement) = parsed
            .protobuf
            .stmts
            .into_iter()
            .next()
            .unwrap()
            .stmt
            .unwrap()
            .node
            .unwrap()
        else {
            panic!("policy expression is not a predicate");
        };
        if let Some(pg_query::NodeEnum::AConst(value)) =
            statement.where_clause.and_then(|node| node.node)
        {
            assert!(
                !matches!(value.val, Some(pg_query::protobuf::a_const::Val::Boolval(value)) if value.boolval)
            );
        }
    }
}

/// Authority functions never grant execution through PUBLIC.
#[test]
#[ignore = "requires: postgres, bin:psql"]
fn authority_derivations_are_not_public_execute() {
    let denied = query(
        &fixture().db_url,
        "SELECT (NOT EXISTS (SELECT FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace CROSS JOIN LATERAL aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) acl WHERE n.nspname='wamn_authority' AND acl.grantee=0 AND acl.privilege_type='EXECUTE'))::text",
    );
    assert!(denied.parse::<bool>().unwrap());
}
