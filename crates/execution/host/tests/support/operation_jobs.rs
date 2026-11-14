//! Operation authority is checked through real control database roles.
use super::*;
use wamn_catalog::HostHandler;

#[tokio::test]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn operations_refuse_unrelated_project_authority_before_execution() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("operation_routes")?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&mut admin, database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "SET app.user_id='{}'",
            PlatformComponent::Provisioning.principal_id()
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let owner = create_or_reuse_user(&mut admin, "job-owner@example.test", "Owner")
        .await?
        .principal_id;
    let other = create_or_reuse_user(&mut admin, "job-other@example.test", "Other")
        .await?
        .principal_id;
    for (principal, project) in [(&owner, "billing"), (&other, "shop")] {
        invite_member(
            &mut admin,
            principal,
            ORG,
            &MemberGrants {
                project_admins: vec![project.into()],
                ..MemberGrants::default()
            },
        )
        .await?;
    }
    let (_server, nats) = wamn_runtime::auth_cache::test_util::nats().await;
    let writer = Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?));
    let delivery = HostRouteDelivery::control(HostRouteHandlers::Control {
        status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
        administration: None,
        control: wamn_postgres::pool(&control_url, 4).await?,
        writer,
        identity: None,
        org: ORG.into(),
        changes: Publisher::new(nats),
    });
    for (id, attachment) in HostRouteSet::Control
        .attachments()
        .filter(|(_, attachment)| {
            attachment.route.handler == HostHandler::OperationJob
                || matches!(
                    attachment.route.operation,
                    "environment.configure"
                        | "environment.delete"
                        | "environment.operation_status"
                        | "pat.revoke"
                )
        })
    {
        let mut value = json!({"project":"billing","env":"dev"});
        match attachment.route.operation {
            "pat.revoke" => value = json!({"token_prefix":"refused-prefix"}),
            "environment.configure" => value["document"] = json!({}),
            "application.deploy" => {
                value["application"] = "billing_app".into();
                value["release"] = format!("sha256:{}", "a".repeat(64)).into();
            }
            "application.rollback" | "application.stop" | "schema.apply" | "schema.diff" => {
                value["application"] = "billing_app".into()
            }
            "operation.status" | "environment.operation_status" => {
                value["job"] = "op-00000000-0000-4000-8000-000000000000".into()
            }
            _ => {}
        }
        let element = if attachment.route.type_.is_read() {
            value
        } else {
            json!({"request_id":"authority","value":value})
        };
        let report = delivery
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(id.into()),
                    delivery_id: "authority".into(),
                    payload: json!([element]).to_string(),
                    caller: None,
                    trace: None,
                    parent_causation: None,
                    if_none_match: None,
                },
                Some(AuthenticatedCaller::new(
                    id,
                    other.to_string(),
                    CredentialType::Session,
                    false,
                    HashSet::new(),
                    None,
                )),
            )
            .await;
        assert!(matches!(
            report.outcome,
            Err(DeliveryError::PermissionDenied(_))
        ));
    }
    Ok(())
}
