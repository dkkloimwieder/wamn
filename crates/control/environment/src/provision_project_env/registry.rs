//! Environment instance creation, registry reads, and control-store projection.

use anyhow::Context as _;
use ring::rand::SecureRandom as _;
use sqlx::Row as _;

use super::{
    INSTANCE_SUFFIX_LEN, Org, Placement, SystemRandom, Triple, cluster_of, read_env_policy,
    validate_instance_suffix,
};

/// Alphabet of the provision-minted instance suffix: `[a-z0-9]`, 36 symbols, so
/// eight of them carry ~41 bits. Narrower than an identity slug's on purpose —
/// the suffix is the LAST bytes of a DNS-1123 label, which must end alphanumeric.
pub(super) const INSTANCE_SUFFIX_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Largest multiple of the alphabet size that fits in a byte (252). A draw at or
/// above it is redrawn rather than folded: a plain `% 36` would over-weight the
/// first four symbols, and the suffix's uniform randomness IS the non-reuse
/// mechanism (wamn-0h0g.13.57) — nothing else keeps a recreated environment off
/// a deleted one's names.
const INSTANCE_SUFFIX_REJECT_AT: usize = 256 - 256 % INSTANCE_SUFFIX_ALPHABET.len();

/// Mint one environment's instance suffix. The randomness is the whole
/// uniqueness mechanism: no naming registry, no collision-retry loop, no
/// derivation from the triple (an owner ruling of wamn-0h0g.13.57).
///
/// The mint lives HERE and not in `wamn-control-provision` because that crate is
/// deliberately pure — no DB, no K8s client, no clock, and no entropy. It takes
/// the suffix as a parameter and derives names from it.
pub(super) fn mint_instance_suffix() -> anyhow::Result<String> {
    let random = SystemRandom::new();
    let mut suffix = String::with_capacity(INSTANCE_SUFFIX_LEN);
    let mut draw = [0_u8; INSTANCE_SUFFIX_LEN];
    while suffix.len() < INSTANCE_SUFFIX_LEN {
        random.fill(&mut draw).map_err(|_| {
            anyhow::anyhow!("operating system could not supply instance-suffix entropy")
        })?;
        for byte in draw {
            if usize::from(byte) >= INSTANCE_SUFFIX_REJECT_AT {
                continue;
            }
            let index = usize::from(byte) % INSTANCE_SUFFIX_ALPHABET.len();
            suffix.push(char::from(INSTANCE_SUFFIX_ALPHABET[index]));
            if suffix.len() == INSTANCE_SUFFIX_LEN {
                break;
            }
        }
    }
    Ok(suffix)
}

/// Derive the target cluster from the org's placement and the named policy.
/// The `env` argument names a policy, which can differ from the environment slug.
/// A pooled org uses its pool. A dedicated org uses the policy's recovery domain
/// through [`cluster_of`]. The caller supplies a registry reader credential.
pub async fn resolve_cluster(system_url: &str, org: &str, env: &str) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = resolve_cluster_on(&mut client, org, env).await;
    drop(client);
    result
}

/// Resolve placement through the supplied system database connection.
pub(crate) async fn resolve_cluster_on(
    client: &mut sqlx::PgConnection,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    let row = sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::select_org_placement_sql()).to_string(),
    ))
    .bind(org)
    .fetch_optional(&mut *client)
    .await
    .context("read org placement")?
    .with_context(|| {
        format!(
            "org {org:?} is not registered: run provision-org before provisioning a project-env"
        )
    })?;
    let placement_type: String = row.get("placement_type");
    let pool: Option<String> = row.get("pool_cluster");
    let placement = match placement_type.as_str() {
        "pooled" => Placement::Pooled {
            pool: pool.context("pooled org row is missing its pool_cluster")?,
        },
        "dedicated" => Placement::Dedicated,
        other => anyhow::bail!("unknown placement_type {other:?} for org {org:?}"),
    };
    let org_obj = Org {
        id: org.to_string(),
        placement,
    };
    // The env must name a policy in the ORG's own set (8df.4 — its recovery
    // domain drives the derivation); a pooled org ignores the policy but the env
    // must still resolve.
    let policy = read_env_policy(client, org, env).await?.with_context(|| {
        format!(
            "env {env:?} names none of org {org:?}'s env policies — provision-org stamps them \
             from a template; customize/add rows in registry.env_policies"
        )
    })?;
    Ok(cluster_of(&org_obj, &policy).name)
}

/// Read one project-env's stored instance suffix from the registry.
pub async fn read_project_env_instance(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = async {
        let env = triple.env.as_str();
        let row = sqlx::query(sqlx::AssertSqlSafe(
            wamn_registry::sql::select_project_env_sql(),
        ))
        .bind(&triple.org)
        .bind(&triple.project)
        .bind(env)
        .fetch_optional(&mut client)
        .await
        .context("read registry.environment_projects row")?
        .with_context(|| format!("project-env {triple} is not recorded"))?;
        let stored: String = row.get("instance_suffix");
        validate_instance_suffix(&stored)
            .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
        Ok(stored)
    }
    .await;
    drop(client);
    result
}

/// Read the policy name of one project-env's registry row. The environment
/// document owns it (docs/architecture/overview.md#environment-lifecycle).
pub async fn read_project_env_policy(system_url: &str, triple: &Triple) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = async {
        let env = triple.env.as_str();
        sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_READ_PROJECT_ENV_POLICY).bind(&triple.org).bind(&triple.project).bind(env).fetch_optional(&mut client)
            .await
            .context("read the policy name of registry.environment_projects")?
            .map(|row| row.get::<String, _>(0))
            .with_context(|| format!("project-env {triple} is not recorded"))
    }
    .await;
    drop(client);
    result
}

/// Record the project and the provisioned project-env in the registry (idempotent).
/// The supplied credential holds the grants used by the registry builders.
///
/// Returns the environment's STORED instance suffix, which is `minted` only on a
/// first provision — see [`do_record_project_env`]. The project derives from
/// the coordinate (`project_env_key`).
#[expect(clippy::too_many_arguments)]
pub(super) async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    policy: Option<&str>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: Option<bool>,
) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = do_record_project_env(
        &mut client,
        triple,
        policy,
        target,
        secret_name,
        secret_namespace,
        minted,
        disposable,
    )
    .await;
    drop(client);
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn do_record_project_env(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
    policy: Option<&str>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: Option<bool>,
) -> anyhow::Result<String> {
    let mut transaction = super::provisioning_transaction(client).await?;
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::upsert_project_sql()).to_string(),
    ))
    .bind(&triple.org)
    .bind(&triple.project)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .context("upsert registry.projects row")?;
    let env = triple.env.as_str();
    let target_id: String = sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_DO_RECORD_PROJECT_ENV).bind(target).fetch_optional(&mut *transaction)
        .await
        .context("read the registry.target row")?
        .with_context(|| format!("target {target:?} is not in registry.target"))?
        .get(0);
    let row = sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::upsert_project_env_with_policy_sql()).to_string(),
    ))
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(env)
    .bind(secret_name)
    .bind(secret_namespace)
    .bind(minted)
    .bind(disposable)
    .bind(policy.unwrap_or(env))
    .bind(&target_id)
    .fetch_one(&mut *transaction)
    .await
    .context("upsert registry.environment_projects row")?;
    transaction
        .commit()
        .await
        .context("commit registry coordinates")?;
    // Read-or-mint: the upsert RETURNS the STORED suffix, which is the freshly
    // minted one on a first provision and the EXISTING one when this triple was
    // already provisioned — the upsert deliberately never refreshes it, because
    // re-minting would orphan every resource the old suffix named. The registry
    // is a trust boundary, so the value is re-validated before any name derives
    // from it.
    let stored: String = row.get(0);
    validate_instance_suffix(&stored)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    Ok(stored)
}

/// Read the pending database without allocating or changing its identity.
pub(crate) async fn pending_database(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<Option<serde_json::Value>> {
    let row = sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_PENDING_DATABASE,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .fetch_optional(&mut *client)
    .await?
    .with_context(|| format!("project-env {triple} is not recorded"))?;
    Ok(row.get(0))
}

/// Record immutable reset inputs outside the database that CNPG will recreate.
pub(crate) async fn record_reset(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
    active: &str,
    inputs: &serde_json::Value,
) -> anyhow::Result<()> {
    let mut tx = super::provisioning_transaction(client).await?;
    let row = sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_RECORD_RESET_2,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("project-env {triple} is not recorded"))?;
    anyhow::ensure!(
        row.get::<bool, _>(1),
        "project-env {triple} is not disposable"
    );
    anyhow::ensure!(
        row.get::<String, _>(0) == active,
        "active database changed during preparation"
    );
    let pending: Option<serde_json::Value> = row.get(2);
    if let Some(pending) = pending {
        anyhow::ensure!(
            pending["inputs"] == *inputs,
            "pending database has different immutable inputs"
        );
    } else {
        let identity: String = sqlx::query("SELECT clock_timestamp()::text")
            .fetch_one(&mut *tx)
            .await?
            .get(0);
        let pending = serde_json::json!({"initialization_identity":identity, "inputs":inputs});
        sqlx::query(
            wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_RECORD_RESET,
        )
        .bind(&triple.org)
        .bind(&triple.project)
        .bind(triple.env.as_str())
        .bind(&pending)
        .execute(&mut *tx)
        .await
        .map(|result| result.rows_affected())?;
    }
    tx.commit().await?;
    Ok(())
}

/// Complete the reset after SQL, CDC and event disposal, before writers resume.
pub async fn finish_database(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
    pending: &serde_json::Value,
) -> anyhow::Result<()> {
    let mut tx = super::provisioning_transaction(client).await?;
    let changed = sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_FINISH_DATABASE,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .bind(pending)
    .execute(&mut *tx)
    .await
    .map(|result| result.rows_affected())?;
    anyhow::ensure!(
        changed == 1,
        "active or pending database changed during reconnect"
    );
    tx.commit().await?;
    Ok(())
}

/// A partially reconnected environment admits no new deployment or SQL operation.
pub async fn require_connected(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<()> {
    require_no_reset(client, triple).await
}

/// Once reset inputs are pinned, deployment waits for the reset to finish.
pub(crate) async fn require_no_reset(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<()> {
    let row = sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_REQUIRE_NO_RESET,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .fetch_optional(&mut *client)
    .await?;
    anyhow::ensure!(
        row.is_none_or(|row| row.get::<bool, _>(0)),
        "database reset is pending; retry wamn schema reset"
    );
    Ok(())
}
