//! The two identity replies needed by the database install sequence.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::{TlsAcceptor, rustls};
use wamn_environment_control::invite::{InviteRequest, invite};
use wamn_environment_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_environment_control::provision_project_env::{
    OrgWorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction, run_org_workload_action,
};
use wamn_identity_client::PatIssuerConfig;
use wamn_platform_identity::{auth_changes::AuthChanges, org::MemberGrants};
use wamn_test_infrastructure::nats_tls::Authority;

pub async fn step_4_19_1_owner_invite(
    system_url: &str,
    org: ProvisionOrgRequest,
    work: &Path,
) -> anyhow::Result<()> {
    let record = org.record.as_ref().unwrap();
    let owner = record.admin.clone();
    let org_id = org.org.clone();
    provision_org(org)
        .await
        .context("4.19.1 provision-org --admin owner")?;
    let endpoint = url::Url::parse(system_url)?;
    run_org_workload_action(&OrgWorkloadActionRequest {
        org: org_id.clone(),
        system_database_url: system_url.into(),
        db_host: Some(endpoint.host_str().unwrap().into()),
        db_port: endpoint.port().unwrap(),
        namespace: "hosts".into(),
        action: WorkloadGenerationAction {
            family: wamn_control_provision::WorkloadRoleFamily::Control,
            verb: WorkloadActionVerb::Prepare,
            generation: wamn_control_provision::CredentialGeneration::A,
        },
        secret: Some(work.join("control.json")),
    })
    .await
    .context("4.19.1 provision-org prepares the control credential as postgres")?;
    let (client, connection) = tokio_postgres::connect(system_url, tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(connection);
    let principal: String = client
        .query_one(
            "SELECT id::text FROM identity.principals WHERE email=$1",
            &[&owner],
        )
        .await?
        .get(0);
    let tls_directory = work.join("invitation-tls");
    std::fs::create_dir(&tls_directory)?;
    let authority = Authority::new(&tls_directory)?;
    let operator = authority.client("operator-install-sequence")?;
    let certificates =
        CertificateDer::pem_file_iter(&authority.server_cert)?.collect::<Result<Vec<_>, _>>()?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from_pem_file(&authority.ca)?)?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::clone(&provider),
    )
    .build()?;
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            certificates,
            PrivateKeyDer::from_pem_file(&authority.server_key)?,
        )?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let identity = PatIssuerConfig {
        endpoint: Some(format!("https://{}", listener.local_addr()?)),
        client_cert: Some(operator.cert),
        client_key: Some(operator.key),
        server_ca: Some(authority.ca),
    };
    // Identity's transport is a fixture. The invitation guard, transaction,
    // memberships, and grants use the real database and library functions.
    let expected_principal = principal.clone();
    let responder = tokio::spawn(async move {
        for (route, body, status, reply) in [
            (
                "users",
                json!({"email":" Owner@Example.test ","display_name":"Owner"}),
                "200 OK",
                json!({"principal_id":expected_principal,"enrolled":false}),
            ),
            (
                "invitations",
                json!({"principal_id":expected_principal}),
                "201 Created",
                json!({"status":"accepted_for_delivery"}),
            ),
        ] {
            let (stream, _) = listener.accept().await?;
            let mut stream = acceptor.accept(stream).await?;
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            let headers_end = loop {
                let count = stream.read(&mut buffer).await?;
                ensure!(
                    count > 0 && request.len() < 8192,
                    "incomplete invitation request"
                );
                request.extend_from_slice(&buffer[..count]);
                if let Some(offset) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..headers_end])?;
            ensure!(
                headers.starts_with(&format!("POST /{route} HTTP/1.1\r\n")),
                "unexpected identity route: {headers}"
            );
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>())
                })
                .context("identity request has no content length")??;
            while request.len() < headers_end + length {
                let count = stream.read(&mut buffer).await?;
                ensure!(count > 0, "incomplete invitation body");
                request.extend_from_slice(&buffer[..count]);
            }
            ensure!(
                serde_json::from_slice::<Value>(&request[headers_end..])? == body,
                "unexpected identity body"
            );
            let reply = reply.to_string();
            stream.write_all(format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len(),
            ).as_bytes()).await?;
            stream.shutdown().await?;
        }
        anyhow::Ok(())
    });
    let request = InviteRequest {
        email: " Owner@Example.test ".into(),
        display_name: "Owner".into(),
        org: org_id,
        grants: MemberGrants {
            org_admin: true,
            ..Default::default()
        },
        system_database_url: system_url.into(),
        identity,
    };
    let invited = invite(&request, &AuthChanges::default())
        .await
        .context("4.19.1 owner_invite_passes_after_provision_org_admin")?;
    ensure!(
        invited.principal_id.as_str() == principal && invited.invitation.unwrap().status == 201,
        "4.19.1 owner invitation did not reuse the provisioned owner"
    );
    tokio::time::timeout(Duration::from_secs(10), responder).await???;
    let other = InviteRequest {
        email: "other@example.test".into(),
        ..request
    };
    let refusal = invite(&other, &AuthChanges::default()).await.unwrap_err();
    ensure!(
        refusal.to_string()
            == format!(
                "org {} already has an active org admin; use user.invite",
                other.org
            ),
        "4.19.1 another email bypassed the first-admin guard: {refusal:#}"
    );
    drop(client);
    driver.await??;
    Ok(())
}
