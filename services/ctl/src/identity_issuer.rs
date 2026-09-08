//! Prepare, retire, or abort a scoped identity database credential.
//!
//! Preparation writes a Secret only after the new login authenticates. It
//! does not install the Secret or access session signing keys. Retirement
//! requires a live session on the replacement generation.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use anyhow::Context as _;
use chrono::{SecondsFormat, Utc};
use clap::{ArgGroup, Args};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::json;
use tokio_postgres::{Client, Config, GenericClient, NoTls, Row};
use url::Url;
use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_DATABASE, IDENTITY_ISSUER_READ_COLUMNS, IDENTITY_ISSUER_ROLE,
    IDENTITY_ISSUER_TABLES, identity_issuer_generation_role, parse_identity_issuer_url,
    prepare_identity_issuer_generation_sql, retire_identity_issuer_generation_sql,
    validate_identity_issuer,
};
use wamn_control_provision::sql;

/// Provisioning inputs for one identity authority, not a project environment.
#[derive(Args)]
#[command(group(ArgGroup::new("identity_generation_action").required(true).multiple(false)
    .args(["prepare_generation", "retire_generation", "abort_generation"])))]
pub struct IdentityIssuerArgs {
    /// Exact HTTPS issuer configured on wamn-identity.
    #[arg(long)]
    pub issuer: String,
    /// Administrator URL for the wamn_system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
    /// Prepare an inactive A/B credential slot.
    #[arg(long, requires = "emit_secret")]
    pub prepare_generation: Option<CredentialGeneration>,
    /// Retire a slot after its replacement has a live session.
    #[arg(long)]
    pub retire_generation: Option<CredentialGeneration>,
    /// Abort an unused prepared slot that has no live sessions.
    #[arg(long)]
    pub abort_generation: Option<CredentialGeneration>,
    /// Write the prepared credential Secret atomically with mode 0600.
    #[arg(long, requires = "prepare_generation")]
    pub emit_secret: Option<PathBuf>,
    /// Namespace for the emitted Secret.
    #[arg(long, default_value = "wamn-system")]
    pub namespace: String,
    /// Name for the emitted Secret, matching the identity chart.
    #[arg(long, default_value = "wamn-identity-db")]
    pub secret_name: String,
}

impl fmt::Debug for IdentityIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityIssuerArgs")
            .field("system_database_url", &"[REDACTED]")
            .field("prepare_generation", &self.prepare_generation)
            .field("retire_generation", &self.retire_generation)
            .field("abort_generation", &self.abort_generation)
            .finish_non_exhaustive()
    }
}

fn admin_config(args: &IdentityIssuerArgs) -> anyhow::Result<Config> {
    validate_identity_issuer(&args.issuer)?;
    let url = Url::parse(&args.system_database_url)
        .map_err(|_| anyhow::anyhow!("system administrator credential must be a URL"))?;
    anyhow::ensure!(
        matches!(url.scheme(), "postgres" | "postgresql")
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.path() == "/wamn_system"
            && url.query().is_none()
            && url.fragment().is_none()
            && !args.system_database_url.chars().any(char::is_whitespace),
        "system administrator credential must name wamn_system with no query or fragment"
    );
    anyhow::ensure!(
        [
            args.prepare_generation,
            args.retire_generation,
            args.abort_generation
        ]
        .iter()
        .filter(|action| action.is_some())
        .count()
            == 1,
        "select exactly one identity generation action"
    );
    anyhow::ensure!(
        args.prepare_generation.is_some() == args.emit_secret.is_some(),
        "only preparation requires --emit-secret"
    );
    for (name, value, maximum) in [
        ("namespace", &args.namespace, 63),
        ("secret name", &args.secret_name, 253),
    ] {
        anyhow::ensure!(
            !value.is_empty()
                && value.len() <= maximum
                && value.split('.').all(|label| !label.is_empty()
                    && label.len() <= 63
                    && label
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'))
                && (name != "namespace" || !value.contains('.')),
            "invalid identity credential {name}"
        );
    }
    args.system_database_url
        .parse()
        .map_err(|_| anyhow::anyhow!("system administrator credential cannot be parsed"))
}

async fn connect(
    config: &Config,
) -> anyhow::Result<(
    Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
)> {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .map_err(|_| anyhow::anyhow!("identity database connection failed"))?;
    Ok((client, tokio::spawn(connection)))
}

/// Run a provisioning action without reading or minting session signing keys.
pub async fn run(args: IdentityIssuerArgs) -> anyhow::Result<()> {
    let config = admin_config(&args)?;
    let (mut admin, connection) = connect(&config).await?;
    let result = async {
        let row = admin
            .query_one(
                "SELECT current_database(), rolsuper FROM pg_roles WHERE rolname = current_user",
                &[],
            )
            .await?;
        anyhow::ensure!(
            row.get::<_, String>(0) == IDENTITY_ISSUER_DATABASE && row.get::<_, bool>(1),
            "identity credential provisioning requires the system database administrator"
        );
        admin
            .query_one(sql::workload_scope_lock_sql(), &[&IDENTITY_ISSUER_ROLE])
            .await?;
        if let Some(generation) = args.prepare_generation {
            prepare(&mut admin, &config, &args, generation).await?;
            println!(
                "prepared identity database generation {}",
                generation.as_str()
            );
        } else {
            let (generation, abort) = if let Some(generation) = args.retire_generation {
                (generation, false)
            } else {
                (
                    args.abort_generation
                        .context("identity generation action is required")?,
                    true,
                )
            };
            retire(&mut admin, &args.issuer, generation, abort).await?;
            println!(
                "{} identity database generation {}",
                if abort { "aborted" } else { "retired" },
                generation.as_str()
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(admin);
    let _ = connection.await;
    result
}

async fn state(client: &(impl GenericClient + Sync), role: &str) -> anyhow::Result<Option<Row>> {
    client
        .query_opt(sql::workload_generation_state_sql(), &[&role])
        .await
        .context("read identity database role state")
}

fn restricted(row: &Row) -> bool {
    [
        "rolsuper",
        "rolcreaterole",
        "rolcreatedb",
        "rolreplication",
        "rolbypassrls",
    ]
    .into_iter()
    .all(|field| !row.get::<_, bool>(field))
        && row.get::<_, i64>("owned_objects") == 0
}

fn active(row: &Row) -> bool {
    restricted(row)
        && row.get::<_, bool>("rolcanlogin")
        && row.get::<_, bool>("rolinherit")
        && row.get::<_, bool>("password_set")
        && row.get::<_, bool>("valid_until_finite")
        && row.get::<_, Vec<String>>("memberships") == [IDENTITY_ISSUER_ROLE]
        && row.get::<_, bool>("membership_options_exact")
        && row.get::<_, Vec<String>>("member_roles").is_empty()
        && row.get::<_, Vec<String>>("connect_databases") == [IDENTITY_ISSUER_DATABASE]
}

fn inactive(row: &Row) -> bool {
    restricted(row)
        && !row.get::<_, bool>("rolcanlogin")
        && row.get::<_, bool>("rolinherit")
        && !row.get::<_, bool>("password_set")
        && row.get::<_, Option<String>>("valid_until").as_deref() == Some("1970-01-01T00:00:00Z")
        && row.get::<_, Vec<String>>("memberships").is_empty()
        && row.get::<_, Vec<String>>("member_roles").is_empty()
        && row.get::<_, Vec<String>>("connect_databases").is_empty()
        && row.get::<_, i64>("sessions") == 0
}

#[derive(Clone, Copy)]
enum Grants {
    None,
    Generation,
    Stable,
    StableBeforePrepare,
}

async fn exact_grants(
    client: &(impl GenericClient + Sync),
    role: &str,
    expected: Grants,
) -> anyhow::Result<()> {
    let rows = client
        .query(sql::role_database_acl_inventory_sql(), &[&role])
        .await?;
    let schemas: BTreeSet<_> = std::iter::once("identity")
        .chain(
            IDENTITY_ISSUER_READ_COLUMNS
                .iter()
                .map(|(schema, _, _)| *schema),
        )
        .collect();
    // The foundation shipped exactly identity USAGE plus key-table CRUD.
    // Recognize that complete old surface only at preparation preflight;
    // a partial upgrade or any extra grant remains unexpected drift.
    let foundation = matches!(expected, Grants::StableBeforePrepare)
        && rows.len() == 1 + IDENTITY_ISSUER_TABLES.len() * 4;
    let count = match expected {
        Grants::None => 0,
        Grants::Generation => 1,
        Grants::StableBeforePrepare if foundation => 1 + IDENTITY_ISSUER_TABLES.len() * 4,
        Grants::Stable | Grants::StableBeforePrepare => {
            schemas.len()
                + IDENTITY_ISSUER_TABLES.len() * 4
                + IDENTITY_ISSUER_READ_COLUMNS
                    .iter()
                    .map(|(_, _, columns)| columns.len())
                    .sum::<usize>()
        }
    };
    anyhow::ensure!(
        rows.len() == count
            && rows.iter().all(|row| {
                let kind: &str = row.get("object_kind");
                let schema: &str = row.get("schema_name");
                let object: &str = row.get("object_name");
                let privilege: &str = row.get("privilege_type");
                !row.get::<_, bool>("is_grantable")
                    && match expected {
                        Grants::None => false,
                        Grants::Generation => {
                            kind == "database"
                                && object == IDENTITY_ISSUER_DATABASE
                                && privilege == "CONNECT"
                        }
                        Grants::Stable | Grants::StableBeforePrepare => {
                            kind == "schema"
                                && if foundation {
                                    schema == "identity"
                                } else {
                                    schemas.contains(schema)
                                }
                                && object == schema
                                && privilege == "USAGE"
                                || kind == "relation"
                                    && schema == "identity"
                                    && IDENTITY_ISSUER_TABLES.contains(&object)
                                    && ["SELECT", "INSERT", "UPDATE", "DELETE"].contains(&privilege)
                                || !foundation
                                    && kind == "column"
                                    && privilege == "SELECT"
                                    && IDENTITY_ISSUER_READ_COLUMNS.iter().any(
                                        |(expected_schema, table, columns)| {
                                            schema == *expected_schema
                                                && columns.iter().any(|column| {
                                                    object == format!("{table}.{column}")
                                                })
                                        },
                                    )
                        }
                    }
            }),
        "identity role has unexpected direct privileges"
    );
    let foreign: bool = client.query_one(
        "SELECT EXISTS (SELECT FROM pg_shdepend d JOIN pg_roles r ON r.oid = d.refobjid \
         WHERE d.refclassid = 'pg_authid'::regclass AND r.rolname = $1 \
           AND d.dbid NOT IN (0, (SELECT oid FROM pg_database WHERE datname = current_database()))) \
         OR EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a \
           JOIN pg_roles r ON r.oid = a.grantee WHERE r.rolname = $1 \
           AND (d.datname <> 'wamn_system' OR a.privilege_type <> 'CONNECT' OR a.is_grantable))",
        &[&role],
    ).await?.get(0);
    anyhow::ensure!(
        !foreign,
        "identity role has authority outside its system database scope"
    );
    Ok(())
}

async fn stable(client: &(impl GenericClient + Sync), before_prepare: bool) -> anyhow::Result<()> {
    if let Some(row) = state(client, IDENTITY_ISSUER_ROLE).await? {
        let members: Vec<String> = row.get("member_roles");
        anyhow::ensure!(
            restricted(&row)
                && !row.get::<_, bool>("rolcanlogin")
                && !row.get::<_, bool>("rolinherit")
                && !row.get::<_, bool>("password_set")
                && row.get::<_, Vec<String>>("memberships").is_empty()
                && row.get::<_, Vec<String>>("connect_databases").is_empty()
                && row.get::<_, bool>("generation_children_exact")
                && row.get::<_, i64>("sessions") == 0
                && members.iter().all(|role| {
                    role.strip_prefix("wamn_identity_issuer_")
                        .is_some_and(|suffix| {
                            let bytes = suffix.as_bytes();
                            bytes.len() == 42
                                && matches!(&bytes[40..], b"_a" | b"_b")
                                && bytes[..40].iter().all(|byte| {
                                    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
                                })
                        })
                }),
            "identity stable role has unexpected attributes or memberships"
        );
        exact_grants(
            client,
            IDENTITY_ISSUER_ROLE,
            if before_prepare {
                Grants::StableBeforePrepare
            } else {
                Grants::Stable
            },
        )
        .await?;
        for member in members {
            exact_grants(client, &member, Grants::Generation).await?;
        }
    }
    Ok(())
}

async fn public_floor(client: &(impl GenericClient + Sync)) -> anyhow::Result<()> {
    anyhow::ensure!(
        client
            .query(sql::public_connect_databases_sql(), &[])
            .await?
            .is_empty(),
        "prepare the cluster PUBLIC CONNECT floor before provisioning identity credentials"
    );
    anyhow::ensure!(
        !client
            .query_one(sql::public_temporary_on_current_database_sql(), &[])
            .await?
            .get::<_, bool>(0),
        "revoke PUBLIC TEMPORARY on wamn_system before provisioning identity credentials"
    );
    Ok(())
}

async fn prepare(
    admin: &mut Client,
    config: &Config,
    args: &IdentityIssuerArgs,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let role = identity_issuer_generation_role(&args.issuer, generation)?;
    let other_role = identity_issuer_generation_role(&args.issuer, generation.other())?;
    let transaction = admin.transaction().await?;
    public_floor(&transaction).await?;
    stable(&transaction, true).await?;
    let target = state(&transaction, &role).await?;
    let other = state(&transaction, &other_role).await?;
    anyhow::ensure!(
        target.as_ref().is_none_or(inactive),
        "prepare requires an inactive identity credential slot"
    );
    anyhow::ensure!(
        (generation == CredentialGeneration::A && other.is_none())
            || other.as_ref().is_some_and(active),
        "initial identity generation must be a, or the opposite generation must be active"
    );
    exact_grants(&transaction, &role, Grants::None).await?;
    if other.is_some() {
        exact_grants(&transaction, &other_role, Grants::Generation).await?;
    }
    let mut random = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("operating system could not supply credential entropy"))?;
    let password = hex::encode(random);
    // Match the existing provisioning lifetime for database credentials, not JWTs.
    let expires_at =
        (Utc::now() + chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
    transaction
        .batch_execute(&prepare_identity_issuer_generation_sql(
            &args.issuer,
            generation,
            &password,
            &expires_at,
        )?)
        .await
        .map_err(|_| anyhow::anyhow!("prepare identity database credential failed"))?;
    // The accepted old surface must have converged completely before commit.
    stable(&transaction, false).await?;
    transaction.commit().await?;
    let publish = async {
        let mut connection_config = config.clone();
        connection_config.user(&role).password(&password);
        let (probe, task) = connect(&connection_config).await?;
        let observed = probe.query_one("SELECT current_user::text, current_database()::text", &[]).await;
        drop(probe);
        let _ = task.await;
        let observed = observed.context("authenticate prepared identity credential")?;
        anyhow::ensure!(observed.get::<_, String>(0) == role && observed.get::<_, String>(1) == IDENTITY_ISSUER_DATABASE,
            "prepared identity credential authenticated with an unexpected scope");
        stable(admin, false).await?;
        let prepared = state(admin, &role).await?.context("prepared identity credential disappeared")?;
        anyhow::ensure!(active(&prepared) && prepared.get::<_, Option<String>>("valid_until").as_deref() == Some(expires_at.as_str()),
            "prepared identity credential has an unexpected state");
        exact_grants(admin, &role, Grants::Generation).await?;
        let mut url = Url::parse(&args.system_database_url).expect("administrator URL was checked before I/O");
        url.set_username(&role).map_err(|_| anyhow::anyhow!("cannot encode identity credential user"))?;
        url.set_password(Some(&password)).map_err(|_| anyhow::anyhow!("cannot encode identity credential password"))?;
        let checked = parse_identity_issuer_url(url.as_str(), &args.issuer)?;
        let document = json!({
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": args.secret_name, "namespace": args.namespace,
                "labels": {"app.kubernetes.io/name": "wamn-identity", "app.kubernetes.io/managed-by": "wamn"}},
            "stringData": {"url": checked.url()}
        });
        crate::provision_project_env::write_secret_json(args.emit_secret.as_deref().context("--emit-secret is required")?, &document)
    }.await;
    if let Err(error) = publish {
        deactivate(admin, &args.issuer, generation)
            .await
            .context("rollback prepared identity credential after publication failure")?;
        return Err(error);
    }
    Ok(())
}

async fn deactivate(
    admin: &Client,
    issuer: &str,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let role = identity_issuer_generation_role(issuer, generation)?;
    admin
        .batch_execute(&retire_identity_issuer_generation_sql(issuer, generation)?)
        .await?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(&role))
        .await?;
    let row = state(admin, &role)
        .await?
        .context("retired identity credential disappeared")?;
    anyhow::ensure!(
        inactive(&row),
        "identity credential did not reach inactive state"
    );
    exact_grants(admin, &role, Grants::None).await
}

async fn retire(
    admin: &mut Client,
    issuer: &str,
    generation: CredentialGeneration,
    abort: bool,
) -> anyhow::Result<()> {
    let role = identity_issuer_generation_role(issuer, generation)?;
    let replacement_role = identity_issuer_generation_role(issuer, generation.other())?;
    let transaction = admin.transaction().await?;
    public_floor(&transaction).await?;
    stable(&transaction, false).await?;
    let old = state(&transaction, &role)
        .await?
        .context("identity credential does not exist")?;
    anyhow::ensure!(active(&old), "identity credential is not active");
    exact_grants(&transaction, &role, Grants::Generation).await?;
    if abort {
        anyhow::ensure!(
            old.get::<_, i64>("sessions") == 0,
            "an in-use identity credential cannot be aborted"
        );
    } else {
        let replacement = state(&transaction, &replacement_role)
            .await?
            .context("replacement identity credential does not exist")?;
        anyhow::ensure!(
            active(&replacement) && replacement.get::<_, i64>("sessions") > 0,
            "replacement identity credential requires an exact active role and a live session"
        );
        exact_grants(&transaction, &replacement_role, Grants::Generation).await?;
    }
    transaction
        .batch_execute(&retire_identity_issuer_generation_sql(issuer, generation)?)
        .await?;
    transaction.commit().await?;
    deactivate(admin, issuer, generation).await
}

#[cfg(test)]
mod tests {
    use super::{IdentityIssuerArgs, admin_config};
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: IdentityIssuerArgs,
    }

    fn args() -> Vec<&'static str> {
        vec![
            "issuer",
            "--issuer",
            "https://identity.wamn-system.svc",
            "--system-database-url",
            "postgres://admin:hidden-value@sysdb/wamn_system",
            "--prepare-generation",
            "a",
            "--emit-secret",
            "identity.json",
        ]
    }

    #[test]
    fn scoped_prepare_arguments_parse_and_redact_admin_credentials() {
        let args = Cli::try_parse_from(args()).unwrap().args;
        assert!(admin_config(&args).is_ok());
        assert_eq!(args.namespace, "wamn-system");
        assert_eq!(args.secret_name, "wamn-identity-db");
        assert!(!format!("{args:?}").contains("hidden-value"));
    }

    #[test]
    fn actions_and_secret_output_are_not_ambiguous() {
        for extra in [
            vec!["--retire-generation", "b"],
            vec!["--abort-generation", "b"],
            vec!["--org", "acme"],
        ] {
            let mut values = args();
            values.extend(extra);
            assert!(Cli::try_parse_from(values).is_err());
        }
        let mut values = args();
        values.truncate(7);
        assert!(Cli::try_parse_from(values.iter().copied()).is_err());
        values.truncate(5);
        assert!(Cli::try_parse_from(values.iter().copied()).is_err());
        values.extend(["--retire-generation", "a"]);
        assert!(Cli::try_parse_from(values).is_ok());
    }

    #[test]
    fn wrong_admin_database_and_issuer_are_refused_before_io() {
        let mut args = Cli::try_parse_from(args()).unwrap().args;
        args.system_database_url = "postgres://admin:hidden-value@sysdb/postgres".into();
        let error = admin_config(&args).unwrap_err();
        assert!(!format!("{error:?}").contains("hidden-value"));
        args.issuer = "http://identity".into();
        assert!(admin_config(&args).is_err());
    }
}
