//! Every restricted login of the system database is refused all it was not granted.

pub mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use anyhow::{Context as _, Result, bail, ensure};
use postgres_protocol::escape::{escape_identifier, escape_literal};
use sqlx::{Connection as _, PgConnection};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_install::provision_org::{OrgLogin, create_env_worker_login};
use wamn_test_infrastructure::database::{self, PASSWORD, System};

/// Log in as `login`, then check its catalog-derived refusals.
async fn login_refusals(system: &System, login: &str, password: &str) -> anyhow::Result<usize> {
    let mut admin = system.admin().await?;
    common::assert_unelevated(&mut admin, login, false).await?;
    let mut actor = common::log_in(&system.login_url(login, password)?).await?;
    common::assert_login_refusals(
        &mut admin,
        &system.postgres,
        login,
        password,
        &mut actor,
        &[],
    )
    .await
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn system_migration_login_refusals() -> anyhow::Result<()> {
    let system = database::system().await?;
    let login = system.declared_login(wamn_role::SYSTEM_ROLE)?.to_owned();
    let mut actor = common::log_in(&system.login_url(&login, PASSWORD)?).await?;
    wamn_postgres::migration_scope::MigrationDatabase::System
        .require_login(&mut actor)
        .await?;
    let checked = login_refusals(&system, &login, PASSWORD).await?;
    common::assert_mapped_logins_are_platform_components(&mut system.admin().await?).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn identity_issuer_login_refusals() -> anyhow::Result<()> {
    let system = database::system().await?;
    let login = system
        .declared_login(wamn_role::IDENTITY_ISSUER_ROLE)?
        .to_owned();
    let mut admin = system.admin().await?;
    common::assert_no_grant_options(&mut admin, &login).await?;
    let writes = common::writes_outside(&mut admin, &login, Some("identity")).await?;
    assert!(
        writes.is_empty(),
        "{login} writes outside identity storage: {writes:?}"
    );
    let may_change_or_read: bool = sqlx::query_scalar(
        "SELECT has_any_column_privilege($1::name, 'identity.authentication_event', 'SELECT,UPDATE')
             OR has_table_privilege($1::name, 'identity.authentication_event', 'DELETE,TRUNCATE')",
    )
    .bind(&login)
    .fetch_one(&mut admin)
    .await?;
    assert!(!may_change_or_read);
    let mut issuer = common::log_in(&system.login_url(&login, PASSWORD)?).await?;
    sqlx::raw_sql(
        "INSERT INTO identity.authentication_event (event_type, outcome, ip_address, ip_source)
         VALUES ('password_sign_in', 'unknown_email', '192.0.2.42', 'socket')",
    )
    .execute(&mut issuer)
    .await?;
    // login_refusals exercises the catalog-derived SELECT, UPDATE, DELETE and TRUNCATE denials.
    // Use the owner for the same row so ACL and RLS denials cannot mask a missing trigger.
    let mut owner = admin.begin().await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_system")
        .execute(&mut *owner)
        .await?;
    let event_id: i64 = sqlx::query_scalar("SELECT id FROM identity.authentication_event")
        .fetch_one(&mut *owner)
        .await?;
    owner.commit().await?;
    for command in [
        "UPDATE identity.authentication_event SET outcome = 'succeeded' WHERE id = $1",
        "DELETE FROM identity.authentication_event WHERE id = $1",
    ] {
        let mut owner = admin.begin().await?;
        sqlx::raw_sql("SET LOCAL ROLE wamn_system")
            .execute(&mut *owner)
            .await?;
        let error = sqlx::query(command)
            .bind(event_id)
            .execute(&mut *owner)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("55000")
        );
        owner.rollback().await?;
    }
    let checked = login_refusals(&system, &login, PASSWORD).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn control_login_refusals() -> anyhow::Result<()> {
    let system = database::system().await?;
    let org = system.org_login(wamn_role::CONTROL_ROLE)?;
    common::assert_no_grant_options(&mut system.admin().await?, org.login.as_str()).await?;
    let checked = login_refusals(&system, org.login.as_str(), &org.password).await?;
    println!("{}: {checked} refused actions", org.login);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn apply_login_refusals() -> anyhow::Result<()> {
    let system = database::system().await?;
    let org = system.org_login(wamn_role::APPLY_ROLE)?;
    common::assert_no_grant_options(&mut system.admin().await?, org.login.as_str()).await?;
    let checked = login_refusals(&system, org.login.as_str(), &org.password).await?;
    println!("{}: {checked} refused actions", org.login);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn pooler_auth_login_refusals() -> anyhow::Result<()> {
    let system = database::system().await?;
    let login = "wamn_pooler_auth";
    // The Pooler looks logins up in the postgres database.
    let mut admin = wamn_postgres::connect(system.postgres.admin_url()).await?;
    common::assert_unelevated(&mut admin, login, false).await?;
    common::assert_no_grant_options(&mut admin, login).await?;
    let mut actor =
        common::log_in(&system.postgres.database_url("postgres", login, PASSWORD)?).await?;
    let checked = common::assert_login_refusals(
        &mut admin,
        &system.postgres,
        login,
        PASSWORD,
        &mut actor,
        &[],
    )
    .await?;

    // The lookup answers only for a login of its own group.
    let worker = create_env_worker_login(
        &mut system.admin().await?,
        &"acme".parse()?,
        &"dev".parse()?,
        false,
        None,
    )
    .await?;
    let lookup = "SELECT count(*) FROM wamn_pooler.lookup_login($1, $2)";
    let control = system.org_login(wamn_role::CONTROL_ROLE)?.login.as_str();
    let apply = system.org_login(wamn_role::APPLY_ROLE)?.login.as_str();
    let migration = system.declared_login(wamn_role::SYSTEM_ROLE)?;
    for (candidate, group, found) in [
        (control, wamn_role::CONTROL_ROLE, 1),
        (apply, wamn_role::APPLY_ROLE, 1),
        (worker.login.as_str(), wamn_role::APPLY_ROLE, 1),
        (worker.login.as_str(), wamn_role::CONTROL_ROLE, 0),
        (apply, wamn_role::CONTROL_ROLE, 0),
        (migration, wamn_role::SYSTEM_ROLE, 0),
    ] {
        let rows: i64 = sqlx::query_scalar(lookup)
            .bind(candidate)
            .bind(group)
            .fetch_one(&mut actor)
            .await?;
        assert_eq!(rows, found, "lookup of {candidate} in {group}");
    }
    println!("{login}: {checked} refused actions");
    Ok(())
}

/// `wamn_system_reader` is a group with no deployed login, so the test enters it with SET ROLE.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn system_reader_group_through_set_role() -> anyhow::Result<()> {
    let system = database::system().await?;
    let group = wamn_role::SYSTEM_READER_ROLE;
    let mut admin = system.admin().await?;
    common::assert_unelevated(&mut admin, group, false).await?;
    common::assert_no_grant_options(&mut admin, group).await?;
    let actions = common::refused_actions(&mut admin, group).await?;
    let mut actor = system.admin().await?;
    database::execute(&mut actor, &format!("SET ROLE {group}")).await?;
    let checked = common::assert_refused(&mut actor, group, &actions).await?;
    common::assert_catalog_refusals(&mut admin, group).await?;
    println!("{group}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn public_executes_no_system_security_definer() -> anyhow::Result<()> {
    let system = database::system().await?;
    let checked = common::assert_public_executes_no_definer(&mut system.admin().await?).await?;
    println!("PUBLIC: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn system_extensions_are_the_declared_extensions() -> Result<()> {
    let system = database::system().await?;
    common::assert_extensions(
        &system.postgres,
        &mut system.admin().await?,
        &system.extensions,
    )
    .await
}

/// The second org, whose rows the logins of the installed org probe.
const OTHER_ORG: &str = "globex";
/// The user principals of the two orgs.
const HOME_PRINCIPAL: &str = "00000000-0000-4000-8000-00000000000a";
const OTHER_PRINCIPAL: &str = "00000000-0000-4000-8000-00000000000b";
/// The project and environment of the seed rows of each org.
const PROJECT: &str = "billing";
const ENV: &str = "dev";

/// One org of the cross-org test.
struct Org<'a> {
    name: &'a str,
    principal: PrincipalId,
}

/// One org login and its session.
struct Actor {
    login: String,
    connection: PgConnection,
}

/// The logins of one group in the installed org and in the other org.
struct Pair {
    home: Actor,
    other: Actor,
}

/// One row of an org that the test writes.
struct Seed {
    table: &'static str,
    columns: Vec<(&'static str, String)>,
}

impl Seed {
    fn new(table: &'static str, columns: &[(&'static str, &str)]) -> Self {
        let columns = columns
            .iter()
            .map(|&(name, value)| (name, value.to_owned()))
            .collect();
        Self { table, columns }
    }

    fn insert(&self) -> String {
        let names: Vec<String> = self
            .columns
            .iter()
            .map(|(name, _)| escape_identifier(name))
            .collect();
        let values: Vec<String> = self
            .columns
            .iter()
            .map(|(_, value)| escape_literal(value))
            .collect();
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            names.join(", "),
            values.join(", ")
        )
    }

    /// The condition that selects this row, on the seed columns in `readable`.
    fn filter(&self, readable: &[String]) -> String {
        let terms: Vec<String> = self
            .columns
            .iter()
            .filter(|(name, _)| readable.iter().any(|column| column == name))
            .map(|(name, value)| format!("{} = {}", escape_identifier(name), escape_literal(value)))
            .collect();
        terms.join(" AND ")
    }
}

/// One row of `org` in each table that a `login_org` policy covers, parents first.
fn seeds(org: &Org<'_>, env: &str, command: Option<&str>) -> Vec<Seed> {
    let name = org.name;
    let principal = org.principal.to_string();
    let principal = principal.as_str();
    let email = format!("owner@{name}.example");
    let issuer = format!("https://{name}.example");
    let target = [("org", name), ("project", PROJECT), ("env", env)];
    let at_target = |columns: &[(&'static str, &'static str)]| [&target[..], columns].concat();
    vec![
        Seed::new(
            "identity.principal",
            &[
                ("id", principal),
                ("type", "user"),
                ("subject", &email),
                ("email", &email),
                ("display_name", "Owner"),
            ],
        ),
        Seed::new("registry.org", &[("id", name)]),
        Seed::new(
            "identity.org_membership",
            &[
                ("principal_id", principal),
                ("org", name),
                ("active", "true"),
            ],
        ),
        Seed::new(
            "identity.org_role",
            &[
                ("principal_id", principal),
                ("org", name),
                ("role", "org-admin"),
            ],
        ),
        Seed::new("registry.project", &[("org", name), ("id", PROJECT)]),
        Seed::new(
            "identity.project_role",
            &[
                ("principal_id", principal),
                ("org", name),
                ("project", PROJECT),
                ("role", "project-admin"),
            ],
        ),
        Seed::new("registry.env", &[("org", name), ("env", env)]),
        Seed::new(
            "provisioning.platform_event",
            &[
                ("org", name),
                ("env", env),
                ("project", PROJECT),
                ("command_id", command.unwrap_or(principal)),
                ("principal_id", principal),
                ("event_type", "env_apply"),
                ("outcome", "started"),
            ],
        ),
        Seed::new(
            "registry.project_env",
            &at_target(&[("secret_name", "database"), ("instance_suffix", "a1b2c3d4")]),
        ),
        Seed::new(
            "identity.project_env_membership",
            &[&target[..], &[("principal_id", principal)]].concat(),
        ),
        Seed::new(
            "registry.env_integration",
            &[
                ("org", name),
                ("env", env),
                ("name", "erp"),
                ("version", "1"),
            ],
        ),
        Seed::new("registry.env_app", &at_target(&[("app", "web")])),
        Seed::new(
            "registry.env_app_connection",
            &at_target(&[("app", "web"), ("alias", "erp"), ("connection_name", "erp")]),
        ),
        Seed::new(
            "registry.event_reader",
            &at_target(&[
                ("publication", "capture"),
                ("slot", "capture"),
                ("stream", "capture"),
                ("replication_secret_name", "capture"),
            ]),
        ),
        Seed::new(
            "registry.capture_gap",
            &at_target(&[
                ("slot", "capture"),
                ("start_at", "2026-01-01T00:00:00Z"),
                ("reason", "lost"),
                ("end_lsn", "0/1"),
            ]),
        ),
        Seed::new(
            "identity.password_login",
            &[
                ("principal_id", principal),
                ("issuer", &issuer),
                ("audience", "control"),
                ("authenticated_at", "2026-01-01T00:00:00Z"),
                ("expires_at", "2026-01-01T08:00:00Z"),
                ("renewal_expires_at", "2026-01-01T01:00:00Z"),
            ],
        ),
    ]
}

/// The `login_org` policies of one table.
#[derive(Debug)]
struct Policy {
    /// The checks that the policies scope to the login's org: `INSERT`, `SELECT`, `UPDATE`
    /// and `DELETE` for the rows that a statement sees, `UPDATE CHECK` for the rows it writes.
    scoped: Vec<String>,
    /// The column that the policy compares with the login's org.
    org_column: Option<String>,
}

impl Policy {
    fn scopes(&self, check: &str) -> bool {
        self.scoped.iter().any(|scoped| scoped == check)
    }
}

/// A policy expression is scoped when it tests the session login, as the org login predicate does.
const POLICY_SQL: &str = r"
SELECT format('%I.%I', schemaname, tablename),
       array_remove(ARRAY[
           CASE WHEN bool_and(coalesce(with_check, qual) ILIKE '%session_user%')
                     FILTER (WHERE cmd IN ('ALL', 'INSERT')) THEN 'INSERT' END,
           CASE WHEN bool_and(qual ILIKE '%session_user%')
                     FILTER (WHERE cmd IN ('ALL', 'SELECT')) THEN 'SELECT' END,
           CASE WHEN bool_and(qual ILIKE '%session_user%')
                     FILTER (WHERE cmd IN ('ALL', 'UPDATE')) THEN 'UPDATE' END,
           CASE WHEN bool_and(coalesce(with_check, qual) ILIKE '%session_user%')
                     FILTER (WHERE cmd IN ('ALL', 'UPDATE')) THEN 'UPDATE CHECK' END,
           CASE WHEN bool_and(qual ILIKE '%session_user%')
                     FILTER (WHERE cmd IN ('ALL', 'DELETE')) THEN 'DELETE' END], NULL),
       max(substring(qual FROM '\((\w+) = \( SELECT registry\.login_org\(\)'))
  FROM pg_catalog.pg_policies WHERE policyname LIKE 'login_org%' GROUP BY 1";

/// The columns of `table` on which `login` holds `privilege`, in column order.
async fn columns(
    admin: &mut PgConnection,
    login: &str,
    table: &str,
    privilege: &str,
) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT attname::text FROM pg_catalog.pg_attribute
          WHERE attrelid = $2::regclass AND attnum > 0 AND NOT attisdropped
            AND has_column_privilege($1::name, attrelid, attname, $3)
          ORDER BY attnum",
    )
    .bind(login)
    .bind(table)
    .bind(privilege)
    .fetch_all(&mut *admin)
    .await?)
}

/// Run `statement` as `actor` under `principal`, in a transaction that rolls back.
async fn attempt(
    actor: &mut PgConnection,
    principal: &PrincipalId,
    statement: &str,
) -> Result<Result<u64, sqlx::Error>> {
    let mut transaction = actor.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        principal,
        "",
        "",
        Some("admin:grant-test"),
        None,
    )
    .await?;
    let result = sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(&mut *transaction)
        .await
        .map(|done| done.rows_affected());
    transaction.rollback().await?;
    Ok(result)
}

/// Read one value as `actor` under `principal`, in a transaction that rolls back.
async fn ask<T>(
    actor: &mut PgConnection,
    principal: &PrincipalId,
    query: &str,
) -> Result<Result<T, sqlx::Error>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let mut transaction = actor.begin().await?;
    wamn_postgres::bind_transaction_identity(&mut transaction, principal, "", "", None, None)
        .await?;
    let result = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&mut *transaction)
        .await;
    transaction.rollback().await?;
    Ok(result)
}

/// Run `statement` as `actor` under `principal`, and commit it.
async fn commit(actor: &mut PgConnection, principal: &PrincipalId, statement: &str) -> Result<()> {
    let mut transaction = actor.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        principal,
        "",
        "",
        Some("admin:grant-test"),
        None,
    )
    .await?;
    database::execute(&mut transaction, statement).await?;
    transaction.commit().await?;
    Ok(())
}

fn expect_value<T: PartialEq + Debug>(
    result: Result<T, sqlx::Error>,
    expected: &T,
    what: &str,
) -> Result<()> {
    let value = result.with_context(|| what.to_owned())?;
    ensure!(
        &value == expected,
        "{what}: {value:?}, expected {expected:?}"
    );
    Ok(())
}

fn expect_code<T: Debug>(result: Result<T, sqlx::Error>, code: &str, what: &str) -> Result<()> {
    match result {
        Ok(value) => bail!("{what}: accepted with {value:?}, expected SQLSTATE {code}"),
        Err(error) => {
            ensure!(
                common::sqlstate(&error).as_deref() == Some(code),
                "{what}: {error}"
            );
            Ok(())
        }
    }
}

/// How a function answers for another org.
#[derive(Clone, Copy, Debug)]
enum Refusal {
    False,
    Denied,
}

/// A call of `function` for the rows of `org`, which a login of that org answers with true.
fn org_call(function: &str, org: &Org<'_>) -> Option<(String, Refusal)> {
    let name = escape_literal(org.name);
    let principal = escape_literal(&org.principal.to_string());
    let (project, env) = (escape_literal(PROJECT), escape_literal(ENV));
    Some(match function {
        "registry.configure_integration_type(text,text,jsonb)" => (
            format!(
                "SELECT registry.configure_integration_type({name}, 'integration_type.list', '{{}}') IS NOT NULL"
            ),
            Refusal::Denied,
        ),
        "registry.login_principal(uuid)" => (
            format!("SELECT registry.login_principal({principal})"),
            Refusal::False,
        ),
        "identity.principal_in_login_org_only(uuid)" => (
            format!("SELECT identity.principal_in_login_org_only({principal})"),
            Refusal::False,
        ),
        "identity.project_admin_grant(uuid,text,text)" => (
            format!("SELECT identity.project_admin_grant({principal}, {name}, {project})"),
            Refusal::False,
        ),
        "identity.project_access_grant(uuid,text,text,text)" => (
            format!("SELECT identity.project_access_grant({principal}, {name}, {env}, {project})"),
            Refusal::False,
        ),
        _ => return None,
    })
}

async fn actor(system: &System, login: &OrgLogin) -> Result<Actor> {
    let url = system.login_url(login.login.as_str(), &login.password)?;
    Ok(Actor {
        login: login.login.as_str().to_owned(),
        connection: common::log_in(&url).await?,
    })
}

/// Each org login of the installed org reads, writes and calls nothing of another org:
/// the `login_org` policies, the functions that read the login's org, and the membership
/// trigger. Each probe has a positive control.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn org_logins_reach_no_other_org() -> Result<()> {
    let system = database::system().await?;
    let mut admin = system.admin().await?;
    let home_name: String = sqlx::query_scalar("SELECT id FROM registry.org")
        .fetch_one(&mut admin)
        .await?;
    let other_logins = wamn_platform_install::provision_org::create_org_logins(
        &mut admin,
        &OTHER_ORG.parse()?,
        &system.database,
    )
    .await?;
    let home = Org {
        name: &home_name,
        principal: HOME_PRINCIPAL.parse()?,
    };
    let other = Org {
        name: OTHER_ORG,
        principal: OTHER_PRINCIPAL.parse()?,
    };
    let mut pairs = Vec::new();
    for home_login in &system.orgs {
        let limit: i32 = sqlx::query_scalar("SELECT rolconnlimit FROM pg_roles WHERE rolname=$1")
            .bind(home_login.login.as_str())
            .fetch_one(&mut admin)
            .await?;
        assert_eq!(limit, -1, "{}", home_login.login);
        let (group, _) = home_login
            .login
            .as_str()
            .rsplit_once('_')
            .context("an org login ends in its identity hash")?;
        pairs.push(Pair {
            home: actor(&system, home_login).await?,
            other: actor(&system, database::org_login(&other_logins, group)?).await?,
        });
    }

    let rows: Vec<(String, Vec<String>, Option<String>)> =
        sqlx::query_as(POLICY_SQL).fetch_all(&mut admin).await?;
    let policies: BTreeMap<String, Policy> = rows
        .into_iter()
        .map(|(table, scoped, org_column)| (table, Policy { scoped, org_column }))
        .collect();
    let home_seeds = seeds(&home, ENV, None);
    let other_seeds = seeds(&other, ENV, None);
    let seeded: BTreeSet<&str> = home_seeds.iter().map(|seed| seed.table).collect();
    let policed: BTreeSet<&str> = policies.keys().map(String::as_str).collect();
    ensure!(
        seeded == policed,
        "the seeds cover {seeded:?}, the login_org policies {policed:?}"
    );

    // Each org writes its rows through its own logins. Before that, each home login inserts
    // its home row, the positive control, and is refused the row of the other org.
    let mut probes = 0;
    for (home_seed, other_seed) in home_seeds.iter().zip(&other_seeds) {
        let table = home_seed.table;
        let mut writer = None;
        for (index, pair) in pairs.iter_mut().enumerate() {
            let insertable = columns(&mut admin, &pair.home.login, table, "INSERT").await?;
            if insertable.is_empty() {
                continue;
            }
            ensure!(
                home_seed
                    .columns
                    .iter()
                    .all(|(name, _)| insertable.iter().any(|column| column == name)),
                "{} inserts only {insertable:?} into {table}",
                pair.home.login
            );
            writer.get_or_insert(index);
            if policies[table].scopes("INSERT") {
                let what = format!("{} inserting into {table}", pair.home.login);
                let own = attempt(
                    &mut pair.home.connection,
                    &home.principal,
                    &home_seed.insert(),
                )
                .await?;
                expect_value(own, &1, &what)?;
                let foreign = attempt(
                    &mut pair.home.connection,
                    &home.principal,
                    &other_seed.insert(),
                )
                .await?;
                expect_code(foreign, "42501", &format!("{what} for {}", other.name))?;
                probes += 1;
            }
        }
        if let Some(index) = writer {
            let pair = &mut pairs[index];
            commit(
                &mut pair.home.connection,
                &home.principal,
                &home_seed.insert(),
            )
            .await?;
            commit(
                &mut pair.other.connection,
                &other.principal,
                &other_seed.insert(),
            )
            .await?;
        } else {
            for (org, seed) in [(&home, home_seed), (&other, other_seed)] {
                let statement = if table == "registry.env" {
                    format!(
                        "SELECT registry.provision_env({}, '{ENV}')",
                        escape_literal(org.name)
                    )
                } else {
                    format!("{} ON CONFLICT DO NOTHING", seed.insert())
                };
                commit(&mut admin, &org.principal, &statement).await?;
            }
        }
    }

    // The membership trigger refuses a principal of another org. The home membership
    // written above is the same insert for a principal of the home org.
    let other_principal = other.principal.to_string();
    let joining = Seed::new(
        "identity.org_membership",
        &[
            ("principal_id", &other_principal),
            ("org", home.name),
            ("active", "true"),
        ],
    );
    let mut joins = 0;
    for pair in &mut pairs {
        let insertable = columns(&mut admin, &pair.home.login, joining.table, "INSERT").await?;
        if joining
            .columns
            .iter()
            .all(|(name, _)| insertable.iter().any(|column| column == name))
        {
            let joined = attempt(
                &mut pair.home.connection,
                &home.principal,
                &joining.insert(),
            )
            .await?;
            let what = format!("{} adding a member of {}", pair.home.login, other.name);
            expect_code(joined, "23514", &what)?;
            joins += 1;
        }
    }
    ensure!(joins > 0, "no org login adds members");

    for pair in &mut pairs {
        let login = pair.home.login.as_str();
        let mut deletes = Vec::new();
        for (home_seed, other_seed) in home_seeds.iter().zip(&other_seeds) {
            let table = home_seed.table;
            let policy = &policies[table];
            let readable = columns(&mut admin, login, table, "SELECT").await?;
            let updatable = columns(&mut admin, login, table, "UPDATE").await?;
            let deletable: bool =
                sqlx::query_scalar("SELECT has_table_privilege($1::name, $2::regclass, 'DELETE')")
                    .bind(login)
                    .bind(table)
                    .fetch_one(&mut admin)
                    .await?;
            if readable.is_empty() {
                ensure!(
                    updatable.is_empty() && !deletable,
                    "{login} changes {table} without reading it"
                );
                continue;
            }
            let other_filter = other_seed.filter(&readable);
            ensure!(
                !other_filter.is_empty(),
                "{login} reads no seed column of {table}"
            );
            if policy.scopes("SELECT") {
                let count = format!("SELECT count(*) FROM {table} WHERE {other_filter}");
                let what = format!("{login}: {count}");
                expect_value(
                    ask::<i64>(&mut pair.home.connection, &home.principal, &count).await?,
                    &0,
                    &what,
                )?;
                let seen = ask::<i64>(&mut pair.other.connection, &other.principal, &count).await?;
                expect_value(seen, &1, &format!("{}: {count}", pair.other.login))?;
                probes += 1;
            }
            if policy.scopes("UPDATE") && !updatable.is_empty() {
                let column = updatable
                    .iter()
                    .find(|&column| readable.contains(column))
                    .with_context(|| format!("{login} updates {table} on no column it reads"))?;
                let column = escape_identifier(column);
                let touch = format!("UPDATE {table} SET {column} = {column} WHERE {other_filter}");
                let what = format!("{login}: {touch}");
                expect_value(
                    attempt(&mut pair.home.connection, &home.principal, &touch).await?,
                    &0,
                    &what,
                )?;
                let touched = attempt(&mut pair.other.connection, &other.principal, &touch).await?;
                expect_value(touched, &1, &format!("{}: {touch}", pair.other.login))?;
                probes += 1;
            }
            if policy.scopes("UPDATE CHECK")
                && let Some(column) = policy
                    .org_column
                    .as_ref()
                    .filter(|&column| updatable.contains(column))
            {
                let home_filter = home_seed.filter(&readable);
                let column = escape_identifier(column);
                let to = |org: &str| {
                    format!(
                        "UPDATE {table} SET {column} = {} WHERE {home_filter}",
                        escape_literal(org)
                    )
                };
                let moved =
                    attempt(&mut pair.home.connection, &home.principal, &to(other.name)).await?;
                expect_code(moved, "42501", &format!("{login}: {}", to(other.name)))?;
                let kept =
                    attempt(&mut pair.home.connection, &home.principal, &to(home.name)).await?;
                expect_value(kept, &1, &format!("{login}: {}", to(home.name)))?;
                probes += 1;
            }
            if policy.scopes("DELETE") && deletable {
                deletes.push(format!("DELETE FROM {table} WHERE {other_filter}"));
            }
        }
        // Children go first, so that each delete of the other org removes its own row.
        deletes.reverse();
        for (actor, principal, rows) in [
            (&mut pair.home, &home.principal, 0),
            (&mut pair.other, &other.principal, 1),
        ] {
            let mut transaction = actor.connection.begin().await?;
            wamn_postgres::bind_transaction_identity(
                &mut transaction,
                principal,
                "",
                "",
                Some("admin:grant-test"),
                None,
            )
            .await?;
            for delete in &deletes {
                let result = sqlx::raw_sql(sqlx::AssertSqlSafe(delete.as_str()))
                    .execute(&mut *transaction)
                    .await
                    .map(|done| done.rows_affected());
                expect_value(result, &rows, &format!("{}: {delete}", actor.login))?;
            }
            transaction.rollback().await?;
        }
        probes += deletes.len();
    }

    let functions: Vec<String> = sqlx::query_scalar(
        r"SELECT p.oid::regprocedure::text FROM pg_catalog.pg_proc p
           WHERE p.prosrc ~ 'login_org\(\)' AND p.oid <> 'registry.login_org()'::regprocedure",
    )
    .fetch_all(&mut admin)
    .await?;
    ensure!(!functions.is_empty(), "no function reads the login's org");
    for function in &functions {
        let (probe, refusal) =
            org_call(function, &other).with_context(|| format!("no call for {function}"))?;
        let (control, _) =
            org_call(function, &home).with_context(|| format!("no call for {function}"))?;
        for pair in &mut pairs {
            let login = pair.home.login.as_str();
            let executes: bool = sqlx::query_scalar(
                "SELECT has_function_privilege($1::name, $2::regprocedure, 'EXECUTE')",
            )
            .bind(login)
            .bind(function)
            .fetch_one(&mut admin)
            .await?;
            if !executes {
                continue;
            }
            let answer = ask::<bool>(&mut pair.home.connection, &home.principal, &probe).await?;
            let what = format!("{login}: {probe}");
            match refusal {
                Refusal::False => expect_value(answer, &false, &what)?,
                Refusal::Denied => expect_code(answer, "42501", &what)?,
            }
            let own = ask::<bool>(&mut pair.home.connection, &home.principal, &control).await?;
            expect_value(own, &true, &format!("{login}: {control}"))?;
            let theirs = ask::<bool>(&mut pair.other.connection, &other.principal, &probe).await?;
            expect_value(theirs, &true, &format!("{}: {probe}", pair.other.login))?;
            probes += 1;
        }
    }
    println!("{probes} cross-org probes, {joins} membership probes");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn env_worker_reaches_only_its_own_env() -> Result<()> {
    let system = database::system().await?;
    let mut admin = system.admin().await?;
    let home = Org {
        name: "acme",
        principal: HOME_PRINCIPAL.parse()?,
    };
    let sibling_env = "prod";
    let worker =
        create_env_worker_login(&mut admin, &home.name.parse()?, &ENV.parse()?, false, None)
            .await?;
    let login = worker.login.as_str();
    let mut connection = common::log_in(&system.login_url(login, &worker.password)?).await?;
    let sibling_command = "00000000-0000-4000-8000-00000000000c";
    let own_seeds = seeds(&home, ENV, None);
    let sibling_seeds = seeds(&home, sibling_env, Some(sibling_command));
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_class c
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'env' AND NOT a.attisdropped
         WHERE c.relkind = 'r' AND has_any_column_privilege($1::name, c.oid, 'SELECT')",
    )
    .bind(login)
    .fetch_all(&mut admin)
    .await?;
    ensure!(
        !tables.is_empty(),
        "the worker can read no environment tables"
    );
    ensure!(
        tables
            .iter()
            .all(|table| own_seeds.iter().any(|seed| seed.table == table)),
        "an environment table lacks fixtures: {tables:?}"
    );

    let mut provisioning =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("SELECT registry.provision_env($1, $2)")
        .bind(home.name)
        .bind(sibling_env)
        .execute(&mut *provisioning)
        .await?;
    provisioning.commit().await?;
    ensure!(
        columns(&mut admin, login, "registry.env", "INSERT")
            .await?
            .is_empty(),
        "the worker can insert an environment mapping"
    );

    let mut deletes = Vec::new();
    for (own, sibling) in own_seeds.iter().zip(&sibling_seeds) {
        let table = own.table;
        let env_table = tables.iter().any(|candidate| candidate == table);
        match table {
            "registry.org" => continue,
            "registry.env" => {}
            _ => {
                if env_table
                    && !columns(&mut admin, login, table, "INSERT")
                        .await?
                        .is_empty()
                {
                    expect_value(
                        attempt(&mut connection, &home.principal, &own.insert()).await?,
                        &1,
                        table,
                    )?;
                    expect_code(
                        attempt(&mut connection, &home.principal, &sibling.insert()).await?,
                        "42501",
                        table,
                    )?;
                }
                commit(&mut admin, &home.principal, &own.insert()).await?;
                if env_table {
                    commit(&mut admin, &home.principal, &sibling.insert()).await?;
                }
            }
        }
        if env_table {
            deletes.extend(
                check_env_rows(
                    &mut admin,
                    &mut connection,
                    login,
                    &home.principal,
                    own,
                    sibling,
                )
                .await?,
            );
        }
    }

    let mut transaction = connection.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        &home.principal,
        "",
        "",
        Some("admin:grant-test"),
        None,
    )
    .await?;
    for (delete, rows) in deletes.iter().rev() {
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(delete.as_str()))
            .execute(&mut *transaction)
            .await
            .map(|done| done.rows_affected());
        expect_value(result, rows, delete)?;
    }
    transaction.rollback().await?;

    let org_event = format!(
        "INSERT INTO provisioning.platform_event (command_id, event_type, outcome, principal_id, org)
         VALUES (uuidv7(), 'env_apply', 'started', '{HOME_PRINCIPAL}', '{}')", home.name,
    );
    expect_code(
        attempt(&mut connection, &home.principal, &org_event).await?,
        "42501",
        &org_event,
    )?;
    check_env_project_access(&mut admin, &mut connection, login, &home, sibling_env).await
}

async fn check_env_rows(
    admin: &mut PgConnection,
    worker: &mut PgConnection,
    login: &str,
    principal: &PrincipalId,
    own: &Seed,
    sibling: &Seed,
) -> Result<Vec<(String, u64)>> {
    let table = own.table;
    let readable = columns(admin, login, table, "SELECT").await?;
    let updatable = columns(admin, login, table, "UPDATE").await?;
    let deletable: bool =
        sqlx::query_scalar("SELECT has_table_privilege($1::name, $2::regclass, 'DELETE')")
            .bind(login)
            .bind(table)
            .fetch_one(admin)
            .await?;
    let mut deletes = Vec::new();
    for (seed, rows) in [(own, 1), (sibling, 0)] {
        let filter = seed.filter(&readable);
        let select = format!("SELECT count(*) FROM {table} WHERE {filter}");
        expect_value(
            ask::<i64>(worker, principal, &select).await?,
            &i64::try_from(rows)?,
            &select,
        )?;
        if !updatable.is_empty() {
            let column = updatable
                .iter()
                .find(|column| readable.contains(column))
                .with_context(|| format!("{login} updates {table} on no column it reads"))?;
            let column = escape_identifier(column);
            let update = format!("UPDATE {table} SET {column} = {column} WHERE {filter}");
            expect_value(attempt(worker, principal, &update).await?, &rows, &update)?;
        }
        if deletable {
            deletes.push((format!("DELETE FROM {table} WHERE {filter}"), rows));
        }
    }
    if updatable.iter().any(|column| column == "env") {
        let filter = own.filter(&readable);
        let env = sibling
            .columns
            .iter()
            .find(|(column, _)| *column == "env")
            .with_context(|| format!("missing sibling environment value for {table}"))?;
        let update = format!(
            "UPDATE {table} SET env = {} WHERE {filter}",
            escape_literal(&env.1)
        );
        expect_code(attempt(worker, principal, &update).await?, "42501", &update)?;
    }
    Ok(deletes)
}

async fn check_env_project_access(
    admin: &mut PgConnection,
    worker: &mut PgConnection,
    login: &str,
    home: &Org<'_>,
    sibling_env: &str,
) -> Result<()> {
    let access_functions: Vec<String> = sqlx::query_scalar(
        "SELECT p.proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = 'identity' AND p.prosecdef AND oidvectortypes(p.proargtypes) = 'uuid, text, text, text'
           AND has_function_privilege($1::name, p.oid, 'EXECUTE')",
    ).bind(login).fetch_all(admin).await?;
    ensure!(
        !access_functions.is_empty(),
        "the worker has no project access functions"
    );
    for name in access_functions {
        for (env, allowed) in [(ENV, true), (sibling_env, false)] {
            let query = format!(
                "SELECT identity.{}('{HOME_PRINCIPAL}', '{}', '{env}', '{PROJECT}')",
                escape_identifier(&name),
                home.name
            );
            expect_value(
                ask::<bool>(worker, &home.principal, &query).await?,
                &allowed,
                &query,
            )?;
        }
    }
    Ok(())
}
