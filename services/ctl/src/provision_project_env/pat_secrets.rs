//! Provisioning PAT issuance, revocation, and Secret documents.

use anyhow::Context as _;

use super::{
    Duration, IdentityErrorKind, NoTls, PatClient, Path, Principal, PrincipalKind,
    PrincipalStatus, Triple, Value, assign_project_role, authenticate_pat, create_service,
    json, resolve_subject, revoke_pat, route_caller_subject, write_secret_json,
};

/// Provisioning PATs retain their existing 30-day lifetime.
pub(super) const PAT_TTL: Duration = Duration::from_secs(2_592_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PatPurpose {
    purpose: &'static str,
    subject_stem: &'static str,
    display_stem: &'static str,
    role: &'static str,
    secret_stem: &'static str,
}

pub(super) const MANAGEMENT_AUTHOR: PatPurpose = PatPurpose {
    purpose: "management-author",
    subject_stem: "wamn-management-author",
    display_stem: "WAMN management author",
    role: "project-author",
    secret_stem: "wamn-pat-management-author",
};

pub(super) const ROUTE_CALLER: PatPurpose = PatPurpose {
    purpose: "route-caller",
    subject_stem: "wamn-route-caller",
    display_stem: "WAMN route caller",
    role: "route-caller",
    secret_stem: "wamn-pat-route-caller",
};

impl PatPurpose {
    pub(super) fn subject(self, triple: &Triple) -> anyhow::Result<String> {
        if self == ROUTE_CALLER {
            return route_caller_subject(&triple.org, &triple.project, triple.env.as_str())
                .context("derive the canonical route-caller subject");
        }
        Ok(format!(
            "{}-{}--{}--{}",
            self.subject_stem, triple.org, triple.project, triple.env
        ))
    }

    pub(super) fn display_name(self, triple: &Triple) -> String {
        format!(
            "{} {}/{}/{}",
            self.display_stem, triple.org, triple.project, triple.env
        )
    }

    fn secret_name(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.secret_stem, triple.org, triple.project, triple.env
        )
    }
}

pub(super) async fn issue_pat_secrets(
    system_url: &str,
    pat_client: &PatClient,
    triple: &Triple,
    namespace: &str,
    management_author_path: Option<&Path>,
    route_caller_path: Option<&Path>,
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT issuance")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT issuance")?;
        if let Some(path) = management_author_path {
            issue_pat_secret(
                &client,
                pat_client,
                triple,
                namespace,
                MANAGEMENT_AUTHOR,
                path,
            )
            .await?;
        }
        if let Some(path) = route_caller_path {
            issue_pat_secret(&client, pat_client, triple, namespace, ROUTE_CALLER, path).await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

async fn issue_pat_secret(
    client: &tokio_postgres::Client,
    pat_client: &PatClient,
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    path: &Path,
) -> anyhow::Result<()> {
    let subject = purpose.subject(triple)?;
    let display_name = purpose.display_name(triple);
    let principal = resolve_or_create_service(client, &subject, &display_name).await?;
    anyhow::ensure!(
        principal.status() == PrincipalStatus::Active,
        "service principal {subject:?} is disabled"
    );
    assign_project_role(
        client,
        principal.id(),
        &triple.org,
        &triple.project,
        purpose.role,
    )
    .await
    .with_context(|| format!("assign {} role", purpose.role))?;

    let issued = pat_client
        .issue(principal.id(), purpose.purpose, PAT_TTL)
        .await
        .with_context(|| format!("issue {} PAT", purpose.purpose))?;
    let authenticated = authenticate_pat(client, &issued.token)
        .await
        .with_context(|| format!("authenticate newly issued {} PAT", purpose.purpose))?
        .with_context(|| format!("newly issued {} PAT did not authenticate", purpose.purpose))?;
    anyhow::ensure!(
        authenticated.principal() == &principal,
        "newly issued {} PAT authenticated as an unexpected principal",
        purpose.purpose
    );

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
    println!(
        "wrote {} ({} PAT Secret; kubectl apply)",
        path.display(),
        purpose.purpose
    );
    Ok(())
}

async fn resolve_or_create_service(
    client: &tokio_postgres::Client,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<Principal> {
    if let Some(principal) = resolve_subject(client, PrincipalKind::Service, subject)
        .await
        .context("resolve service principal")?
    {
        return Ok(principal);
    }

    match create_service(client, subject, display_name).await {
        Ok(principal) => Ok(principal),
        Err(error) if error.kind() == IdentityErrorKind::Conflict => {
            resolve_subject(client, PrincipalKind::Service, subject)
                .await
                .context("resolve concurrently created service principal")?
                .context("service principal conflict was not resolvable")
        }
        Err(error) => Err(error).context("create service principal"),
    }
}

pub(super) async fn revoke_provisioning_pat(system_url: &str, prefix: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT revocation")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT revocation")?;
        revoke_pat(&client, prefix)
            .await
            .context("revoke PAT by prefix")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

pub(super) fn render_pat_secret(
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    principal_id: &str,
    token: &str,
    prefix: &str,
    expires_at: &str,
) -> anyhow::Result<Value> {
    let subject = purpose.subject(triple)?;
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": purpose.secret_name(triple),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-pat",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
            "annotations": {
                "wamn.io/credential-purpose": purpose.purpose,
                "wamn.io/principal-id": principal_id,
                "wamn.io/principal-kind": "service",
                "wamn.io/principal-subject": subject,
                "wamn.io/project-role": purpose.role,
                "wamn.io/pat-prefix": prefix,
                "wamn.io/pat-expires-at": expires_at,
            },
        },
        "type": "Opaque",
        "stringData": {
            "token": token,
        },
    }))
}

pub(super) fn parse_pat_prefix(value: &str) -> Result<String, String> {
    let valid = value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !valid {
        return Err("PAT prefix must be 16 lowercase hex digits".to_owned());
    }
    Ok(value.to_owned())
}
