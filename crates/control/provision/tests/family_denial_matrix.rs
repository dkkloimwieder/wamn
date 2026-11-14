//! Database authority through real reads and refused writes.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use url::Url;

use wamn_control_provision::audit_retention::reconcile_audit_retention_grants_sql;
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{
    PLATFORM_GROUP_ROLE, WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType,
    workload_login,
};
use wamn_control_provision::{CONTROL_PORTABLE_STORE_SQL, SYSTEM_SCHEMA_SQL};

const POSTGRES_INIT: &str = include_str!("../../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

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
const PROJECT_ENV_KEY_A: &str = "t1";
const PROJECT_ENV_KEY_B: &str = "t2";

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
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let script =
        format!("SET app.user_id='{actor}'; SET app.operation='admin:provision-fixture'; {script}");
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
    let mut url = Url::parse(admin_url).expect("the test server URL is a URL");
    url.set_username(role).expect("set the probe role");
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
fn generation(family: WorkloadRoleFamily) -> String {
    let target = match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY_A,
            database: DATABASE,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: DATABASE,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: CONTROL_DATABASE,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: ORG,
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
        .flat_map(|family| [family.acl_role().to_owned(), generation(family)])
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
FOREACH t IN ARRAY ARRAY['t1','t2'] LOOP
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
    apply(&db_url, "seed two projects", SEED);
    apply(
        &db_url,
        "create the logged fixture relations",
        RETENTION_FIXTURE,
    );
    // The apply-package grant path, as it runs after the log triggers.
    apply(
        &db_url,
        "converge the audit retention grants",
        &reconcile_audit_retention_grants_sql(),
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

/// The executor can read schedule configuration but cannot author it.
#[test]
fn the_executor_reads_settings_without_write_authority() {
    let fixture = fixture();
    apply(
        &fixture.db_url,
        "seed schedule configuration",
        "INSERT INTO platform.setting(target_type,target_id,key,value) VALUES ('project','billing','timezone','\"UTC\"'::jsonb)",
    );
    let executor = login_url(
        &fixture.admin,
        &generation(WorkloadRoleFamily::ExecutorPlatform),
    );
    let value: serde_json::Value = serde_json::from_str(&query(&executor,
        "SELECT value::text FROM platform.setting WHERE target_type='project' AND target_id='billing' AND key='timezone'")).unwrap();
    assert!(value.is_string());
    assert!(sqlstate(&executor,
        "UPDATE platform.setting SET value='null'::jsonb WHERE target_type='project' AND target_id='billing' AND key='timezone'").is_some());
    let unchanged: serde_json::Value = serde_json::from_str(&query(&fixture.db_url,
        "SELECT value::text FROM platform.setting WHERE target_type='project' AND target_id='billing' AND key='timezone'")).unwrap();
    assert_eq!(unchanged, value);
}

// ---------------------------------------------------------------------------
// THE PLATFORM-GRAIN ARMS
// ---------------------------------------------------------------------------

/// A PLATFORM FAMILY WITH GRANTS AND NO PROJECT CONTEXT READS EXACTLY ITS GRANTS
/// (`wamn-0h0g.22.17`).
///
/// The project base grants are narrowed `TO wamn_app`, and PostgreSQL DEFAULT-DENIES
/// when RLS is on and no policy matches — so the narrowing does not EXEMPT a
/// platform principal, it LOCKS IT OUT at zero rows with no exception. What
/// admits it is one permissive `TO wamn_platform` arm per relation, and what
/// still BOUNDS it is its own table grants. Both halves are read back here: rows
/// where a grant exists, `42501` where it does not.
#[test]
fn a_platform_family_without_project_env_key_context_reads_exactly_what_its_grants_say() {
    let fixture = fixture();
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
        )
        .parse::<bool>()
        .unwrap(),
        true,
        "the executor-platform generation does not effectively hold \
         {PLATFORM_GROUP_ROLE}: every platform read would be zero rows"
    );

    let as_executor = login_url(&fixture.admin, &login);
    // It has NO project context and structurally cannot acquire one: the session
    // derivation is the guest's, and a platform family holds neither `USAGE` on
    // `wamn_authority` nor `EXECUTE` on it. So BOTH projects' rows are the
    // correct answer below — the confinement is the grant set, not a predicate.
    assert_eq!(
        sqlstate(
            &as_executor,
            "SELECT wamn_authority.current_project_key();\n"
        )
        .as_deref(),
        Some("42501"),
        "a platform family reached the guest session derivation; if it can \
         derive a project key, this arm is measuring a guest"
    );
    assert_eq!(
        query(&as_executor, "SELECT count(*) FROM wamn_run.runs")
            .parse::<u64>()
            .unwrap(),
        2,
        "the executor-platform family reads runs through its declared grants"
    );
}

/// THE PROJECT-TARGETED PLATFORM MEMBER (`wamn-0h0g.22.25`).
///
/// # A premise of that ruling no longer holds
///
/// The ruling named TWO project-targeted `wamn_platform` members, the effect
/// writer and `Retention`. `wamn-0h0g.22.32` then DEMOTED the writer out of the group,
/// and `wamn-0h0g.10.15` removed the effect-writer family, so `Retention` is the
/// only subject the arm still has. That is asserted here
/// rather than remembered, so the day a second one appears this arm reds.
#[test]
fn the_project_env_key_targeted_platform_member_reads_nothing_outside_its_grants() {
    let fixture = fixture();
    let login = generation(WorkloadRoleFamily::Retention);
    let as_retention = login_url(&fixture.admin, &login);
    // Its grant set names ONE relation, and the shared arm is USING (true), so
    // the honest assertion is: every row of that relation, and 42501 everywhere
    // else. The project predicate it keeps in its own statements is what
    // re-narrows it, and that is not a database-enforced boundary.
    assert_eq!(
        query(&as_retention, "SELECT count(*) FROM wamn_run.runs")
            .parse::<u64>()
            .unwrap(),
        2,
        "the retention family cannot read the relation its grant set names"
    );
    for relation in ["wamn_run.run_queue"] {
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

/// The audit retention family reads only the retention columns of a history
/// table whose log keeps entries for n days (`wamn-emtx.13`).
///
/// The minted login counts the P30D history table and reads its three columns.
/// The server refuses an image column, the unlimited history table, and the
/// logged relation itself. The family holds no `wamn_platform` edge.
#[test]
fn the_audit_retention_family_reads_only_the_retention_columns_of_its_history_tables() {
    let fixture = fixture();
    let family = WorkloadRoleFamily::AuditRetention;
    let login = generation(family);
    assert_eq!(
        query(
            &fixture.db_url,
            &format!("SELECT pg_has_role('{login}', '{PLATFORM_GROUP_ROLE}', 'MEMBER')::text"),
        )
        .parse::<bool>()
        .unwrap(),
        false,
        "the audit retention generation reaches {PLATFORM_GROUP_ROLE}"
    );
    let as_retention = login_url(&fixture.admin, &login);
    assert_eq!(
        query(
            &as_retention,
            "SELECT count(*) FROM retention_fixture.entry_history \
              WHERE row_key IS NOT NULL AND position > 0 AND changed_at IS NOT NULL",
        )
        .parse::<u64>()
        .unwrap(),
        0,
        "the audit retention family cannot read the retention columns of the P30D history table"
    );
    for statement in [
        "SELECT before FROM retention_fixture.entry_history;\n",
        "SELECT count(*) FROM retention_fixture.ledger_history;\n",
        "DELETE FROM retention_fixture.ledger_history;\n",
        "SELECT count(*) FROM retention_fixture.entry;\n",
    ] {
        assert_eq!(
            sqlstate(&as_retention, statement).as_deref(),
            Some("42501"),
            "the audit retention family ran {statement}"
        );
    }
}

/// THE GUEST IS THE ONE FAMILY THE MINIMUM CONFINES BY ROW.
///
/// Its ACL says `SELECT` on `wamn_run.runs` and the server agrees; what the ACL
/// cannot say is that it sees ONE project's row out of two. This is the arm that
/// separates an isolated credential from a blind one for the family whose
/// confinement is RLS-shaped rather than grant-shaped.
#[test]
fn the_guest_family_reads_its_own_project_env_key_and_only_its_own() {
    let fixture = fixture();
    let login = generation(WorkloadRoleFamily::App);
    let as_guest = login_url(&fixture.admin, &login);
    assert_eq!(
        query(
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
    // A settable identity must not move it. The minimum keys on `current_user`, and
    // this shows that the retired `app.project_env_key` path is inert for the guest.
    assert_eq!(
        query(
            &as_guest,
            &format!(
                "BEGIN; SET LOCAL app.project_env_key = '{PROJECT_ENV_KEY_B}'; \
                 SELECT count(*) FROM wamn_run.runs; COMMIT"
            ),
        )
        .parse::<u64>()
        .unwrap(),
        1,
        "a session-settable identity moved the guest's row visibility"
    );
}

// ---------------------------------------------------------------------------
// THE PLATFORM GROUP'S MEMBER SET, AND THE ONE RATIFIED EXCEPTION
// ---------------------------------------------------------------------------

#[test]
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
// THE PASS-ALL POLICIES of the control database
// ---------------------------------------------------------------------------

/// Every table of the control database has a project, so no policy admits
/// every row. Read from the server.
#[test]
fn no_control_policy_passes_every_row() {
    let fixture = fixture();
    assert_eq!(
        rows(
            &fixture.control_url,
            "SELECT n.nspname || '.' || c.relname || ' ' || p.polname \
               FROM pg_catalog.pg_policy p \
               JOIN pg_catalog.pg_class c ON c.oid = p.polrelid \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
              WHERE pg_get_expr(p.polqual, p.polrelid) = 'true' \
                 OR pg_get_expr(p.polwithcheck, p.polrelid) = 'true' \
              ORDER BY (n.nspname || '.' || c.relname || ' ' || p.polname) COLLATE \"C\"",
        ),
        Vec::<String>::new(),
        "a pass-all policy exists on a table that has a project"
    );
}

/// A reader can select its public membership fields but cannot read the whole row.
#[test]
fn the_identity_reader_cannot_read_private_membership_fields() {
    let mut target = Url::parse(&fixture().control_url).unwrap();
    target
        .set_username(&generation(WorkloadRoleFamily::IdentityReader))
        .unwrap();
    target.set_password(Some(PROBE_PASSWORD)).unwrap();
    assert!(
        psql(
            target.as_str(),
            "SELECT principal_id,org,status FROM identity.org_memberships LIMIT 0"
        )
        .0
    );
    assert!(
        !psql(
            target.as_str(),
            "SELECT * FROM identity.org_memberships LIMIT 0"
        )
        .0
    );
}

/// Authority functions never grant execution through PUBLIC.
#[test]
fn authority_derivations_are_not_public_execute() {
    let denied = query(
        &fixture().db_url,
        "SELECT NOT EXISTS (SELECT FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace CROSS JOIN LATERAL aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) acl WHERE n.nspname='wamn_authority' AND acl.grantee=0 AND acl.privilege_type='EXECUTE')",
    );
    assert!(denied.parse::<bool>().unwrap());
}
