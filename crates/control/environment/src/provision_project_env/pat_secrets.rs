//! Provisioning PAT issuance, revocation, and Secret documents.

pub(super) mod plan;
pub use plan::parse_pat_prefix;
pub(super) use plan::render_pat_secret;

use anyhow::Context as _;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};

use super::{
    Client, Duration, IdentityErrorType, NoTls, PatClient, Path, PathBuf, Principal,
    PrincipalStatus, PrincipalType, Triple, Value, assign_project_role, authenticate_pat,
    create_service, json, operator_subject, provisioning_transaction, resolve_subject, revoke_pat,
    write_secret_json,
};

/// One PAT `Secret` that provisioning issued and wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedPatSecret {
    /// The PAT purpose, such as `management-author`.
    pub purpose: &'static str,
    /// The file the `Secret` JSON was written to.
    pub path: PathBuf,
}

/// Provisioning PATs retain their existing 30-day lifetime.
pub(super) const PAT_TTL: Duration = Duration::from_hours(720);

pub use plan::PatPurpose;

pub use plan::MANAGEMENT_AUTHOR;

pub(super) use plan::OPERATOR;

pub(super) async fn issue_pat_secrets(
    system_url: &str,
    pat_client: &PatClient,
    triple: &Triple,
    namespace: &str,
    management_author_path: Option<&Path>,
    operator_path: Option<&Path>,
    changes: &AuthChanges,
) -> anyhow::Result<Vec<IssuedPatSecret>> {
    let (mut client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT issuance")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        let mut issued = Vec::new();
        if let Some(path) = management_author_path {
            issued.push(
                issue_pat_secret(
                    &mut client,
                    pat_client,
                    triple,
                    namespace,
                    MANAGEMENT_AUTHOR,
                    path,
                    changes,
                )
                .await?,
            );
        }
        if let Some(path) = operator_path {
            issued.push(
                issue_pat_secret(
                    &mut client,
                    pat_client,
                    triple,
                    namespace,
                    OPERATOR,
                    path,
                    changes,
                )
                .await?,
            );
        }
        Ok::<_, anyhow::Error>(issued)
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

async fn issue_pat_secret(
    client: &mut Client,
    pat_client: &PatClient,
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    path: &Path,
    changes: &AuthChanges,
) -> anyhow::Result<IssuedPatSecret> {
    let (principal, issued) = mint_pat(client, pat_client, triple, purpose, changes).await?;
    let secret = render_pat_secret(
        triple,
        namespace,
        purpose,
        principal.id().as_str(),
        &issued.token,
        &issued.token_prefix,
        &issued.expires_at,
    )?;
    write_secret_json(path, &secret)?;
    Ok(IssuedPatSecret {
        purpose: purpose.purpose,
        path: path.to_path_buf(),
    })
}

pub async fn mint_pat(
    client: &mut Client,
    pat_client: &PatClient,
    triple: &Triple,
    purpose: PatPurpose,
    changes: &AuthChanges,
) -> anyhow::Result<(Principal, wamn_identity_client::PatResponse)> {
    let subject = purpose.subject(triple)?;
    let display_name = purpose.display_name(triple);
    let principal = resolve_or_create_service(client, &subject, &display_name, changes).await?;
    anyhow::ensure!(
        principal.status() == PrincipalStatus::Active,
        "service principal {subject:?} is disabled"
    );
    let transaction = provisioning_transaction(client).await?;
    let drops = assign_pat_role(&transaction, &principal, triple, purpose).await?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} role", purpose.role))?;
    changes.publish(drops).await;

    let issued = pat_client
        .issue(principal.id(), purpose.purpose, PAT_TTL)
        .await
        .with_context(|| format!("issue {} PAT", purpose.purpose))?;
    let authenticated = authenticate_pat(&*client, &issued.token)
        .await
        .with_context(|| format!("authenticate newly issued {} PAT", purpose.purpose))?
        .with_context(|| format!("newly issued {} PAT did not authenticate", purpose.purpose))?;
    anyhow::ensure!(
        authenticated.principal() == &principal,
        "newly issued {} PAT authenticated as an unexpected principal",
        purpose.purpose
    );

    Ok((principal, issued))
}

pub async fn assign_pat_role(
    transaction: &tokio_postgres::Transaction<'_>,
    principal: &Principal,
    triple: &Triple,
    purpose: PatPurpose,
) -> anyhow::Result<AuthDrops> {
    let mut drops = AuthDrops::default();
    assign_project_role(
        transaction,
        principal.id(),
        &triple.org,
        &triple.project,
        purpose.role,
        &mut drops,
    )
    .await
    .with_context(|| format!("assign {} role", purpose.role))?;
    Ok(drops)
}

/// Mint a management-author PAT for a recorded environment without writing it.
///
/// # Errors
///
/// Returns an error if the environment, principal, role, or issuance is invalid.
pub async fn mint_management_author_pat(
    system_url: &str,
    issuer: &wamn_identity_client::PatIssuerConfig,
    triple: &Triple,
    changes: &AuthChanges,
) -> anyhow::Result<wamn_identity_client::PatResponse> {
    let pat_client = PatClient::new(issuer)?;
    super::validate_project_env(&triple.org, &triple.project, triple.env.as_str())?;
    super::read_project_env_instance(system_url, triple).await?;
    let (mut client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT issuance")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        let (_, pat) =
            mint_pat(&mut client, &pat_client, triple, MANAGEMENT_AUTHOR, changes).await?;
        Ok(pat)
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

pub async fn resolve_or_create_service(
    client: &mut Client,
    subject: &str,
    display_name: &str,
    changes: &AuthChanges,
) -> anyhow::Result<Principal> {
    let (principal, drops) = resolve_or_create_service_drops(client, subject, display_name).await?;
    changes.publish(drops).await;
    Ok(principal)
}

async fn resolve_or_create_service_drops(
    client: &mut Client,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<(Principal, AuthDrops)> {
    if let Some(principal) = resolve_subject(&*client, PrincipalType::Service, subject)
        .await
        .context("resolve service principal")?
    {
        return Ok((principal, AuthDrops::default()));
    }

    let transaction = provisioning_transaction(client).await?;
    let mut drops = AuthDrops::default();
    match create_service(&transaction, subject, display_name, &mut drops).await {
        Ok(principal) => {
            transaction
                .commit()
                .await
                .context("commit service principal")?;
            Ok((principal, drops))
        }
        Err(error) if error.error_type() == IdentityErrorType::Conflict => {
            transaction
                .rollback()
                .await
                .context("roll back conflicting service principal")?;
            resolve_subject(&*client, PrincipalType::Service, subject)
                .await
                .context("resolve concurrently created service principal")?
                .context("service principal conflict was not resolvable")
                .map(|principal| (principal, AuthDrops::default()))
        }
        Err(error) => Err(error).context("create service principal"),
    }
}

pub async fn revoke_provisioning_pat(
    system_url: &str,
    prefix: &str,
    changes: &wamn_platform_identity::auth_changes::AuthChanges,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT revocation")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        let transaction = provisioning_transaction(&mut client).await?;
        let mut drops = wamn_platform_identity::auth_changes::AuthDrops::default();
        revoke_pat(&transaction, prefix, &mut drops)
            .await
            .context("revoke PAT by prefix")?;
        transaction
            .commit()
            .await
            .context("commit PAT revocation")?;
        changes.publish(drops).await;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

#[cfg(test)]
mod tests;
