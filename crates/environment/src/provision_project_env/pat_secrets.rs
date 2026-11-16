//! Provisioning PAT issuance, revocation, and Secret documents.
pub(super) mod plan;
pub(super) use plan::render_pat_secret;

use anyhow::Context as _;

use super::{
    Client, Duration, IdentityErrorType, PatClient, Path, PathBuf, Principal, PrincipalStatus,
    PrincipalType, Triple, Value, assign_project_role, authenticate_pat, create_service, json,
    provisioning_transaction, resolve_subject, revoke_pat, write_secret_json,
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
) -> anyhow::Result<Vec<IssuedPatSecret>> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect for PAT issuance")?;

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
                )
                .await?,
            );
        }
        if let Some(path) = operator_path {
            issued.push(
                issue_pat_secret(&mut client, pat_client, triple, namespace, OPERATOR, path)
                    .await?,
            );
        }
        Ok::<_, anyhow::Error>(issued)
    }
    .await;
    drop(client);
    result
}

async fn issue_pat_secret(
    client: &mut Client,
    pat_client: &PatClient,
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    path: &Path,
) -> anyhow::Result<IssuedPatSecret> {
    let (principal, issued) = mint_pat(client, pat_client, triple, purpose).await?;
    let secret = render_pat_secret(
        triple,
        namespace,
        purpose,
        principal.id(),
        &issued.token,
        issued.token_prefix,
        &issued.expires_at,
    );
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
) -> anyhow::Result<(Principal, wamn_identity_protocol::wire::PatResponse)> {
    let subject = purpose.subject(triple);
    let display_name = purpose.display_name(triple);
    let principal = resolve_or_create_service(client, &subject, &display_name).await?;
    anyhow::ensure!(
        principal.status() == PrincipalStatus::Active,
        "service principal {subject:?} is disabled"
    );
    let mut transaction = provisioning_transaction(client).await?;
    assign_pat_role(&mut transaction, &principal, triple, purpose).await?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} role", purpose.role))?;

    let issued = pat_client
        .issue(principal.id(), purpose.purpose, PAT_TTL)
        .await
        .with_context(|| format!("issue {} PAT", purpose.purpose))?;
    let authenticated = authenticate_pat(&mut *client, &issued.token)
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
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    triple: &Triple,
    purpose: PatPurpose,
) -> anyhow::Result<()> {
    assign_project_role(
        transaction,
        principal.id(),
        &triple.org,
        &triple.project,
        purpose.role,
    )
    .await
    .with_context(|| format!("assign {} role", purpose.role))?;
    Ok(())
}

pub async fn resolve_or_create_service(
    client: &mut Client,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<Principal> {
    let mut transaction = provisioning_transaction(client).await?;
    if let Some(principal) = resolve_subject(&mut transaction, PrincipalType::Service, subject)
        .await
        .context("resolve service principal")?
    {
        transaction
            .commit()
            .await
            .context("commit service principal lookup")?;
        return Ok(principal);
    }

    match create_service(&mut transaction, subject, display_name).await {
        Ok(principal) => {
            transaction
                .commit()
                .await
                .context("commit service principal")?;
            Ok(principal)
        }
        Err(error) if error.error_type() == IdentityErrorType::Conflict => {
            transaction
                .rollback()
                .await
                .context("roll back conflicting service principal")?;
            let mut transaction = provisioning_transaction(client).await?;
            let principal = resolve_subject(&mut transaction, PrincipalType::Service, subject)
                .await
                .context("resolve concurrently created service principal")?
                .context("service principal conflict was not resolvable")?;
            transaction
                .commit()
                .await
                .context("commit service principal lookup")?;
            Ok(principal)
        }
        Err(error) => Err(error).context("create service principal"),
    }
}

pub async fn revoke_provisioning_pat(
    system_url: &str,
    prefix: &wamn_identity_protocol::pat_prefix::PatPrefix,
) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect for PAT revocation")?;

    let result = async {
        let mut transaction = provisioning_transaction(&mut client).await?;
        revoke_pat(&mut transaction, prefix)
            .await
            .context("revoke PAT by prefix")?;
        transaction
            .commit()
            .await
            .context("commit PAT revocation")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    result
}
