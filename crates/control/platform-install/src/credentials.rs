//! Native CNPG credentials and public connection configuration.
use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use sqlx::Connection as _;
use std::path::Path;
use std::time::Duration;
use wamn_control_provision::connection::{ManagedRole, managed_role};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_kubernetes::{kubectl, secret};
use wamn_postgres::{ConnectionConfig, SecretRef};
/// Rebuild CNPG declarations from the registry and require the requested role to belong to it.
pub async fn reconcile(
    registry_input: &str,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    declaration: &ManagedRole,
) -> anyhow::Result<()> {
    refresh(
        registry_input,
        kubeconfig,
        context,
        namespace,
        cluster,
        Some(declaration),
        &[],
    )
    .await
}

/// Stop managing the requested logins while their owning deletion removes the registry row.
pub async fn remove_roles(
    registry_input: &str,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    names: &[String],
) -> anyhow::Result<()> {
    refresh(
        registry_input,
        kubeconfig,
        context,
        namespace,
        cluster,
        None,
        names,
    )
    .await
}

async fn refresh(
    registry_input: &str,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    required: Option<&ManagedRole>,
    removing: &[String],
) -> anyhow::Result<()> {
    with_chart_lock(registry_input, namespace, async {
        let mut declarations = crate::roles::registered(registry_input, namespace).await?;
        if let Some(required) = required {
            anyhow::ensure!(
                declarations
                    .get(cluster)
                    .into_iter()
                    .flatten()
                    .any(|role| role.name == required.name
                        && role.password_secret.name == required.password_secret.name),
                "requested database credential has no registry or platform owner"
            );
        }
        for roles in declarations.values_mut() {
            roles.retain(|role| !removing.contains(&role.name));
        }
        let object: Value = kubectl::get(
            kubeconfig,
            context,
            namespace,
            "cluster.postgresql.cnpg.io",
            cluster,
        )
        .await?
        .context("CNPG cluster is absent from the org namespace")?;
        let annotations = &object["metadata"]["annotations"];
        let owner = annotations["meta.helm.sh/release-name"]
            .as_str()
            .context("CNPG Cluster must have a Helm owner")?;
        let owner_namespace = annotations["meta.helm.sh/release-namespace"]
            .as_str()
            .context("CNPG Cluster must have a Helm namespace")?;
        let target = wamn_kubernetes::Target {
            kubeconfig: kubeconfig.into(),
            context: context.into(),
            namespace: owner_namespace.into(),
        };
        let mut values =
            crate::platform::installed::release_values(&target, owner_namespace, owner)
                .await?
                .context("CNPG Helm owner is absent")?;
        let reference = values["platform"]["chart"]
            .as_str()
            .context("CNPG Helm owner has no chart reference")?;
        let chart = crate::platform::artifact_plan::select(Path::new(reference))?;
        prepare(&target, &declarations).await?;
        values["managedRoles"] = crate::roles::values(&declarations)?;
        let directory = tempfile::tempdir()?;
        let entry = crate::environment::entry(
            &chart.directory().join("platform/org"),
            owner_namespace,
            owner,
            values,
        )?;
        crate::platform::resources::apply_release(&target, &entry, directory.path()).await
    })
    .await
}

/// Create all derived credentials outside Helm, preserving existing passwords.
pub async fn prepare(
    target: &wamn_kubernetes::Target,
    declarations: &crate::roles::Declarations,
) -> anyhow::Result<()> {
    for declaration in declarations.values().flatten() {
        let username = declaration.name.as_str();
        let secret_name = declaration.password_secret.name.as_str();
        secret::update(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            secret_name,
            |secret| {
                if secret.metadata.resource_version.is_none() {
                    let mut bytes = [0u8; 32];
                    SystemRandom::new()
                        .fill(&mut bytes)
                        .map_err(|_| anyhow::anyhow!("generate database credential"))?;
                    *secret = secret::Secret::basic_auth(
                        &target.namespace,
                        secret_name,
                        username,
                        &hex::encode(bytes),
                    );
                } else {
                    anyhow::ensure!(
                        secret.content_type == "kubernetes.io/basic-auth",
                        "old database Secret shape refused"
                    );
                    let user = secret
                        .data
                        .get("username")
                        .context("database Secret has no username")?;
                    anyhow::ensure!(
                        STANDARD.decode(user)? == username.as_bytes()
                            && secret.data.contains_key("password"),
                        "database Secret names another role"
                    );
                }
                Ok(())
            },
        )
        .await?;
    }
    Ok(())
}

pub fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: String,
    secret: String,
) -> ConnectionConfig {
    ConnectionConfig {
        host,
        port,
        database,
        username,
        credentials: SecretRef {
            namespace: namespace.into(),
            name: secret,
        },
        server_ca: SecretRef {
            namespace: namespace.into(),
            name: format!("{cluster}-ca"),
        },
    }
}

pub async fn wait_role(connection: &mut sqlx::PgConnection, username: &str) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin)",
        )
        .bind(username)
        .fetch_one(&mut *connection)
        .await?;
        if present {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "CNPG did not reconcile the database login"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Create a managed control-database login and publish its public configuration.
pub async fn provision_control_role(
    input: &str,
    namespace: &str,
    family: WorkloadRoleFamily,
) -> anyhow::Result<()> {
    let options = wamn_postgres::connect_options(input).await?;
    let database = options
        .get_database()
        .context("control database is absent")?;
    let cluster = options
        .get_host()
        .split('.')
        .next()
        .context("control host is absent")?
        .trim_end_matches("-rw");
    let username = workload_login(
        family,
        WorkloadRoleIdentity::Org {
            org: namespace,
            database,
        },
    )?;
    let secret = wamn_control_provision::org_workload_secret_name(family, namespace);
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let mut connection = wamn_postgres::connect(input).await?;
    let surface = sql::stable_surface_sql(family)
        .unwrap_or_else(|| sql::ensure_workload_acl_role_sql(family));
    sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
        .execute(&mut connection)
        .await?;
    reconcile(
        input,
        &kubeconfig,
        "",
        namespace,
        cluster,
        &managed_role(username.clone(), family.acl_role(), secret.clone()),
    )
    .await?;
    wait_role(&mut connection, &username).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::grant_workload_login_sql(
        family, database, &username,
    )))
    .execute(&mut connection)
    .await?;
    let config = configuration(
        namespace,
        cluster,
        options.get_host().into(),
        options.get_port(),
        database.into(),
        username,
        secret,
    );
    let name = wamn_registry::name::control_administration_secret_name(namespace);
    let object = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":namespace},"data":{"control.json":config.to_json()?}});
    kubectl::apply(&kubeconfig, "", namespace, &object).await?;
    Ok(())
}

/// Reconcile the platform scope's own CNPG login and return its public connection configuration.
pub async fn provision_platform_migration(
    registry_input: &str,
    input: &str,
    scope: wamn_control_provision::migrations::MigrationDatabase,
) -> anyhow::Result<String> {
    let source = wamn_postgres::configuration(input)?;
    let config: ConnectionConfig = serde_json::from_str(&source)
        .context("explicit platform initialization requires a separate migration connection")?;
    let cluster = config
        .server_ca
        .name
        .strip_suffix("-ca")
        .context("CNPG CA Secret must identify its cluster")?;
    let role = scope.managed_role(&config.database);
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    reconcile(
        registry_input,
        &kubeconfig,
        "",
        &config.credentials.namespace,
        cluster,
        &role,
    )
    .await?;
    let mut admin = wamn_postgres::connect(input).await?;
    wait_role(&mut admin, &role.name).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT CONNECT ON DATABASE {} TO {}",
        postgres_protocol::escape::escape_identifier(&config.database),
        postgres_protocol::escape::escape_identifier(&role.name)
    )))
    .execute(&mut admin)
    .await?;
    scope.configuration(input)
}

/// Serialize org chart writes and cancel Helm if the registry session is lost.
pub(crate) async fn with_chart_lock<T>(
    registry_input: &str,
    namespace: &str,
    work: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let mut connection = wamn_postgres::connect(registry_input).await?;
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('wamn.org-chart/' || $1, 0))")
        .bind(namespace)
        .execute(&mut connection)
        .await?;
    tokio::pin!(work);
    // The existing lifecycle operations use a one-second heartbeat as well.
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            result = &mut work => { connection.ping().await.context("org chart lock session was lost")?; return result; },
            _ = heartbeat.tick() => connection.ping().await.context("org chart lock session was lost")?,
        }
    }
}
