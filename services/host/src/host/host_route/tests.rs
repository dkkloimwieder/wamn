use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::process::Command;
use wamn_app::app_config::{AppComponent, AppOperation, AppOperationType};
use wamn_authentication::administration::{
    GrantAuthority, GrantChange, create_role, mutate_grant, project_administration,
};
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_engine::{ingress::IngressRouting, operation::WamnOperation};
use wamn_identity_protocol::{
    AuthorityClass, PlatformComponent, PrincipalId, secret::PatToken,
    workload_role::WorkloadRoleFamily,
};
use wamn_postgres::{ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres};
use wash_runtime::host::http::{DynamicRouter, HostHandler, Ingress, IngressRoute, ServiceHttpJob};
use wash_runtime::plugin::HostPlugin;

use super::{AppConfig, Authorization, HostRouteDelivery, OperationHost, Workflow};

const READ: &str = "session-test:purchase/read";
const WRITE: &str = "session-test:purchase/write";
const WORKLOAD: &str = "client-test";

#[tokio::test]
#[ignore = "requires: docker, bin:helm, input:web-install, bin:pnpm"]
async fn generated_app_client_live() -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    let database = wamn_test_infrastructure::database::project().await?;
    let login = database.login_url(WorkloadRoleFamily::Platform.login_name())?;
    let mut connection = wamn_postgres::connect(login.as_str()).await?;
    let config = Arc::new(AppConfig {
        org: "acme".parse()?,
        project: "billing".parse()?,
        environment: "dev".parse()?,
        application: "client-test".parse()?,
        grant_cache_seconds: 60,
        components: vec![AppComponent {
            name: "purchase".into(),
            package: "session_test".parse()?,
            digest: wamn_digest::bytes_sha256(b"client test"),
            connections: Vec::new(),
            schema: None,
            operations: [
                (READ, AppOperationType::Read),
                (WRITE, AppOperationType::Write),
            ]
            .into_iter()
            .map(|(reference, type_)| {
                Ok((
                    format!("{reference}@1.0.0").parse()?,
                    AppOperation {
                        component: "purchase".into(),
                        public: true,
                        host_transaction: true,
                        type_: Some(type_),
                        idempotency_key: None,
                        signed_result: None,
                    },
                ))
            })
            .collect::<anyhow::Result<_>>()?,
            data_change: None,
        }],
    });
    let key = wamn_registry::project_env_key(&config.org, &config.project, &config.environment);
    let admin_id = uuid::Uuid::new_v4();
    let member_id = uuid::Uuid::new_v4();
    let admin_token = PatToken::mint()?;
    let member_token = PatToken::mint()?;
    let mut seed = wamn_test_infrastructure::identity::transaction(
        &mut connection,
        &PlatformComponent::Provisioning.principal_id(),
        "admin:seed-host-client",
    )
    .await?;
    create_role(&mut seed, "purchase-reader").await?;
    for (name, user, token, is_admin) in [
        ("admin", admin_id, &admin_token, true),
        ("member", member_id, &member_token, false),
    ] {
        let service = uuid::Uuid::new_v4();
        // The list shows human users. A service principal presents each PAT.
        sqlx::query(
            r#"INSERT INTO platform."user" (id, issuer, sub, type, email, display_name) VALUES
                 ($1, 'https://identity.example.test', $3, 'user', $4, $3),
                 ($2, 'wamn', $3, 'service', $5, $3)"#,
        )
        .bind(user)
        .bind(service)
        .bind(name)
        .bind(format!("{name}@example.test"))
        .bind(format!("{name}-service@example.test"))
        .execute(&mut *seed)
        .await?;
        for principal in [user, service].map(PrincipalId::from) {
            if is_admin {
                project_administration(&mut seed, &principal, true).await?;
            } else {
                mutate_grant(
                    &mut seed,
                    &key,
                    GrantAuthority::Provisioning,
                    GrantChange::UserRole {
                        user_id: &principal,
                        role: "purchase-reader",
                        grant: true,
                    },
                )
                .await?;
            }
        }
        sqlx::query(
            "INSERT INTO platform.pat (user_id, prefix, hash, label, expires_at)
             VALUES ($1, $2, $3, 'client test', now() + interval '1 hour')",
        )
        .bind(service)
        .bind(token.lookup().to_string())
        .bind(token.digest().to_string())
        .execute(&mut *seed)
        .await?;
    }
    mutate_grant(
        &mut seed,
        &key,
        GrantAuthority::Provisioning,
        GrantChange::Permission {
            role: "purchase-reader",
            reference: READ,
            grant: true,
        },
    )
    .await?;
    seed.commit().await?;

    let pools = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            HashMap::from([(
                config.project.clone(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::Platform, login),
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 2,
                    wait_timeout_ms: 2_000,
                    statement_timeout_ms: 5_000,
                },
            )]),
            None,
        ),
    )));
    let authorization = Arc::new(Authorization::new(Arc::clone(&pools)));
    authorization.configure_app(&config)?;
    let postgres = Arc::new(OperationHost::new(
        Arc::clone(&pools),
        Arc::clone(&authorization),
        None,
    ));
    let routing = IngressRouting::new(Arc::clone(&postgres.scopes)).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::new(Arc::clone(&authorization), None),
    ));
    let operations = Arc::new(WamnOperation::new(
        Arc::clone(&postgres) as Arc<dyn HostPlugin>,
        Arc::clone(&postgres.scopes),
    ));
    let workflow = Workflow::new(
        Arc::clone(&postgres),
        pools,
        operations,
        String::new(),
        false,
    );
    let delivery = HostRouteDelivery::new(postgres, authorization, config);

    let ingress = Ingress::new(DynamicRouter::default(), "127.0.0.1:0".parse()?).await?;
    ingress.start().await?;
    let (sender, mut jobs) = tokio::sync::mpsc::channel::<ServiceHttpJob>(2);
    ingress
        .on_service_http_resolved(WORKLOAD, &[IngressRoute::ingress("127.0.0.1")], sender)
        .await?;
    let server = tokio::spawn(async move {
        while let Some(job) = jobs.recv().await {
            let response = delivery.http(&routing, &workflow, WORKLOAD, job.req).await;
            let _ = job.resp_tx.send(Ok(response));
        }
    });

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/platform_ui");
    let status = Command::new("pnpm")
        .current_dir(directory)
        .args(["exec", "vitest", "run", "test/control-client.live.test.ts"])
        .env(
            "WAMN_CONTROL_CLIENT_BASE_URL",
            format!("http://{}", ingress.addr()),
        )
        .env("WAMN_CONTROL_CLIENT_ADMIN_TOKEN", admin_token.expose())
        .env("WAMN_CONTROL_CLIENT_ADMIN_ID", admin_id.to_string())
        .env("WAMN_CONTROL_CLIENT_MEMBER_TOKEN", member_token.expose())
        .env("WAMN_CONTROL_CLIENT_MEMBER_ID", member_id.to_string())
        .kill_on_drop(true)
        .status()
        .await;
    ingress.on_service_http_unbind(WORKLOAD).await?;
    ingress.stop().await?;
    ingress.stopped().await;
    server.await?;
    let status = status.context("run the generated application client test")?;
    anyhow::ensure!(status.success(), "Vitest exited with {status}");
    Ok(())
}
