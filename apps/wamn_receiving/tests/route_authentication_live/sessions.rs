//! Receiving sessions tests and helpers.

use super::*;


/// Prepare one session route without changing the checked-in PAT publication.
#[tokio::test]
#[ignore = "requires the completed disposable Receiving journey and WAMN_SESSION_HOST_FIXTURE_OUTPUT"]
async fn production_receiving_session_host_fixture() -> anyhow::Result<()> {
    const SESSION_ATTACHMENT: &str = "purchase-order-get-http";
    const SESSION_ROLE: &str = "session-host-reader";
    const REQUEST_ID: &str = "session-host-proof";
    const ORDER_ID: &str = "00000000-0000-0000-0000-000000000301";
    const SESSION_RELEASE_ID: u32 = RELEASE_ID + 1;

    let inputs = JourneyDocument::required()?;
    let output = required_journey_path("WAMN_SESSION_HOST_FIXTURE_OUTPUT")?;
    anyhow::ensure!(
        output.is_absolute() && !output.exists(),
        "session fixture output must be a new absolute path in the private journey directory"
    );
    let scratch = ScratchRoot::create()?;
    let (admin, admin_task) = connect(&inputs.system_pg_url).await?;
    let instance_suffix: String = admin
        .query_one(
            "SELECT instance_suffix FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("read the existing Receiving environment instance")?
        .get(0);
    let triple = wamn_control_registry::Triple::new(ORG, PROJECT, ENVIRONMENT);
    let audience =
        wamn_control_provision::session_target::session_audience(&triple, &instance_suffix)?;
    let database = wamn_control_provision::project_env_database_name(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &instance_suffix,
    );
    let mut project_url = reqwest::Url::parse(&inputs.system_pg_url)?;
    project_url.set_path(&format!("/{database}"));
    let (project, project_task) = connect(project_url.as_str()).await?;
    let previous = project
        .query_one(
            "SELECT releases.verified_publisher_principal, snapshots.canonical_bytes \
             FROM catalog.effective_releases AS releases \
             JOIN catalog.release_manifest_v3_snapshots AS snapshots \
               USING (tenant_id, effective_release_id) \
             WHERE releases.tenant_id = $1 AND releases.effective_release_id = $2",
            &[&TENANT, &(RELEASE_ID as i32)],
        )
        .await
        .context("read the completed PAT journey release")?;
    let publisher: String = previous.get(0);
    let previous_bytes: Vec<u8> = previous.get(1);
    let previous_release = ReleaseManifestWeld::load_canonical_bytes(
        &previous_bytes,
        "completed PAT journey release",
    )?;
    let mut attachments = Vec::with_capacity(JOURNEY_PACKAGES.len());
    let mut changed = 0;
    for package in JOURNEY_PACKAGES {
        let source = journey_publication_root(package).join("attachments.json");
        let original =
            std::fs::read(&source).with_context(|| format!("read {}", source.display()))?;
        let mut document: Value = serde_json::from_slice(&original)?;
        if let Some(attachment) = document.get_mut(SESSION_ATTACHMENT) {
            anyhow::ensure!(
                attachment["registered-operation"] == OPERATION
                    && attachment["auth-policy"] == serde_json::json!({"modes": ["pat"]}),
                "session fixture target differs from the existing purchase-order GET route"
            );
            attachment["auth-policy"] = serde_json::json!({"modes": ["session"]});
            changed += 1;
        }
        let destination = scratch
            .path()
            .join(format!("{}-attachments.json", package.id));
        std::fs::write(&destination, serde_json::to_vec(&document)?)?;
        anyhow::ensure!(
            std::fs::read(&source)? == original,
            "fixture changed a package publication file"
        );
        attachments.push(destination);
    }
    anyhow::ensure!(
        changed == 1,
        "session fixture must change exactly one copied attachment"
    );
    let (manifest_digest, release) = publish_journey_release(
        &inputs,
        JourneyReleaseTarget {
            project_url: project_url.as_str(),
            system_url: &inputs.system_pg_url,
            publisher: &publisher,
            project: project.as_ref(),
            control: admin.as_ref(),
            release_id: SESSION_RELEASE_ID,
            attachments,
        },
    )
    .await?;
    anyhow::ensure!(
        release.release().effective_release_id == 2 && release.manifest().format_version == 1,
        "session proof must publish format 1 as release 2"
    );
    let mut expected_attachments = previous_release.manifest().attachments.clone();
    expected_attachments
        .get_mut(SESSION_ATTACHMENT)
        .context("PAT release omitted the session fixture target")?
        .auth_policy = serde_json::json!({"modes": ["session"]});
    anyhow::ensure!(
        release.manifest().attachments == expected_attachments
            && release.manifest().components == previous_release.manifest().components
            && release.manifest().wirings == previous_release.manifest().wirings
            && release.manifest().registrations == previous_release.manifest().registrations,
        "session release changed facts beyond the copied attachment policy"
    );

    let human = create_human(
        admin.as_ref(),
        "session-host@example.test",
        "Session host proof",
    )
    .await?;
    project_env_membership::grant(ProjectEnvMembershipArgs {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        principal_id: human.id().to_string(),
        system_database_url: inputs.system_pg_url.clone(),
    })
    .await?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
            &[&TENANT, &SESSION_ROLE],
        )
        .await?;
    project.execute(
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, $2, $3)",
        &[&TENANT, &SESSION_ROLE, &OPERATION],
    ).await?;
    project
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, email, status) \
         VALUES ($1, $2::text::uuid, 'session-host@example.test', 'active')",
            &[&TENANT, &human.id().as_str()],
        )
        .await?;
    project.execute(
        "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ($1, $2::text::uuid, $3)",
        &[&TENANT, &human.id().as_str(), &SESSION_ROLE],
    ).await?;
    let pat = issue_pat(
        admin.as_ref(),
        human.id(),
        "deployed host session proof",
        Duration::from_secs(3600),
    )
    .await?;
    // The PAT journey updates this row. Read its current contract fields independently,
    // rather than treating a stale fixture version or any HTTP 200 as success.
    let expected_value: Value = project.query_one(
        "SELECT jsonb_build_object( \
           'id', id::text, 'purchase_order_number', purchase_order_number, \
           'supplier_id', supplier_id::text, 'status', status, 'row_version', row_version::text, \
           'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
           'updated_at', to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) \
         FROM receiving.purchase_order WHERE id = $1::text::uuid",
        &[&ORDER_ID],
    ).await.context("read the independent expected purchase-order GET result")?.get(0);
    let document = serde_json::json!({
        "manifest_digest": manifest_digest,
        "human_pat": pat.token(),
        "human_id": human.id().as_str(),
        "audience": audience,
        "org": ORG,
        "project": PROJECT,
        "environment": ENVIRONMENT,
        "tenant": TENANT,
        "instance_suffix": instance_suffix,
        "roles": [SESSION_ROLE],
        "route_path": "/purchase_order/get",
        "route_host": inputs.route_host,
        "request_body": [{"request_id": REQUEST_ID, "id": ORDER_ID}],
        "expected_response": [{"request_id": REQUEST_ID, "value": expected_value}],
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .context("create private session-host fixture output")?;
    file.write_all(&serde_json::to_vec(&document)?)?;
    anyhow::ensure!(
        file.metadata()?.permissions().mode() & 0o777 == 0o600,
        "session-host fixture output permissions differ from 0600"
    );
    println!("HOST_SESSION_FIXTURE result=pass manifest_digest={manifest_digest}");
    project_task.abort();
    admin_task.abort();
    Ok(())
}

/// The native driver uses a pinned loopback HTTPS port-forward, not cluster DNS.
/// Separate deployed host Jobs prove cluster transport and key-removal bounds.
#[tokio::test]
#[ignore = "requires the completed Receiving session fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT"]
async fn production_nested_session_call_preserves_original_caller() -> anyhow::Result<()> {
    tokio::time::timeout(
        Duration::from_secs(180),
        nested_session_caller(false, false),
    )
    .await
    .context("nested session proof exceeded 180 seconds")?
}

#[tokio::test]
#[ignore = "requires the fresh-only Receiving fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT"]
async fn production_nested_fresh_only_requires_pat_and_observes_revocation() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(180), nested_session_caller(true, false))
        .await
        .context("nested fresh-only proof exceeded 180 seconds")?
}

#[tokio::test]
#[ignore = "requires the fresh-only Receiving fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT"]
async fn production_session_client_login_and_fresh_selection() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(180), nested_session_caller(true, true))
        .await
        .context("session client proof exceeded 180 seconds")?
}

pub(super) async fn nested_session_caller(fresh_only: bool, client_proof: bool) -> anyhow::Result<()> {
    const ROLE: &str = "session-nested-caller";
    let overlay_attachment = JOURNEY_ATTACHMENTS
        .iter()
        .find(|attachment| attachment.operation == OVERLAY_RECORD_RECEIPT)
        .context("the journey omitted the overlay Receipt route")?;
    let direct_attachment = JOURNEY_ATTACHMENTS
        .iter()
        .find(|attachment| attachment.operation == BASE_RECORD_RECEIPT)
        .context("the journey omitted the direct Receipt route")?;
    let mut selected_attachments = vec![overlay_attachment];
    if fresh_only {
        selected_attachments.push(direct_attachment);
    }
    if client_proof {
        selected_attachments.push(
            JOURNEY_ATTACHMENTS
                .iter()
                .find(|attachment| attachment.operation == OPERATION)
                .context("the journey omitted the ordinary client GET route")?,
        );
    }
    let mut inputs = JourneyDocument::required()?;
    let issuer = required_journey("WAMN_IDENTITY_ISSUER")?;
    let endpoint = required_journey("WAMN_SESSION_NESTED_HTTPS_ENDPOINT")?;
    let endpoint = reqwest::Url::parse(&endpoint)?;
    anyhow::ensure!(
        endpoint.scheme() == "https"
            && matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1"))
            && endpoint.port().is_some()
            && endpoint.path() == "/"
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "nested proof requires an explicit loopback HTTPS port-forward origin"
    );
    let ca = std::fs::read(required_journey_path("WAMN_IDENTITY_CA_FILE")?)?;
    let keys = IssuerKeys::new(IssuerKeysConfig::new(
        &issuer,
        endpoint.join("/.well-known/jwks.json")?.as_str(),
        &ca,
    )?)?;
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .tls_backend_rustls()
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca)?)
        .build()?;
    let scratch = ScratchRoot::create()?;
    inputs.compilation_cache_directory = scratch.path().join("nested-compilation-cache");
    std::fs::create_dir(&inputs.compilation_cache_directory)?;
    let (admin, admin_task) = connect(&inputs.system_pg_url).await?;
    let instance_suffix: String = admin.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
        &[&ORG, &PROJECT, &ENVIRONMENT],
    ).await?.get(0);
    let audience = wamn_control_provision::session_target::session_audience(
        &wamn_control_registry::Triple::new(ORG, PROJECT, ENVIRONMENT),
        &instance_suffix,
    )?;
    let mut project_url = reqwest::Url::parse(&inputs.system_pg_url)?;
    project_url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(
            ORG,
            PROJECT,
            ENVIRONMENT,
            &instance_suffix,
        )
    ));
    let (project, project_task) = connect(project_url.as_str()).await?;
    let previous = project.query_one(
        "SELECT releases.verified_publisher_principal, snapshots.canonical_bytes \
         FROM catalog.effective_releases AS releases \
         JOIN catalog.release_manifest_v3_snapshots AS snapshots USING (tenant_id, effective_release_id) \
         WHERE releases.tenant_id = $1 AND releases.effective_release_id = 1",
        &[&TENANT],
    ).await?;
    let publisher: String = previous.get(0);
    let previous = ReleaseManifestWeld::load_canonical_bytes(
        &previous.get::<_, Vec<u8>>(1),
        "original Receiving PAT release",
    )?;
    let operation_freshness = |operation: &str| {
        previous
            .manifest()
            .components
            .iter()
            .find_map(|component| component.operations.get(operation))
            .map(|operation| operation.fresh_only)
    };
    anyhow::ensure!(
        operation_freshness(BASE_RECORD_RECEIPT) == Some(fresh_only)
            && operation_freshness(OVERLAY_RECORD_RECEIPT) == Some(false),
        "nested proof requires the admitted base freshness and an ordinary overlay"
    );
    let digests = released_component_digests(&previous, &inputs.route_host)?;
    let deployed_bytes: Vec<u8> = project
        .query_one(
            "SELECT canonical_bytes FROM catalog.release_manifest_v3_snapshots \
         WHERE tenant_id = $1 AND effective_release_id = 2",
            &[&TENANT],
        )
        .await?
        .get(0);
    // The frozen driver checks the registered digest, so this is an actual
    // disposable release 3, not an unregistered in-memory manifest alteration.
    let auth_policy = if fresh_only {
        serde_json::json!({"modes": ["pat", "session"]})
    } else {
        serde_json::json!({"modes": ["session"]})
    };
    let mut attachments = Vec::new();
    let mut changed = 0;
    for package in JOURNEY_PACKAGES {
        let source = journey_publication_root(package).join("attachments.json");
        let original = std::fs::read(&source)?;
        let mut document: Value = serde_json::from_slice(&original)?;
        for selected in &selected_attachments {
            if let Some(attachment) = document.get_mut(selected.id) {
                anyhow::ensure!(
                    attachment["registered-operation"] == selected.operation
                        && attachment["auth-policy"] == serde_json::json!({"modes": ["pat"]}),
                    "caller proof route differs from the authored PAT attachment"
                );
                attachment["auth-policy"] = auth_policy.clone();
                changed += 1;
            }
        }
        let path = scratch
            .path()
            .join(format!("{}-attachments.json", package.id));
        std::fs::write(&path, serde_json::to_vec(&document)?)?;
        anyhow::ensure!(
            std::fs::read(&source)? == original,
            "nested proof changed package source"
        );
        attachments.push(path);
    }
    anyhow::ensure!(
        changed == selected_attachments.len(),
        "caller proof must change exactly the selected copied route policies"
    );
    let (_, release) = publish_journey_release(
        &inputs,
        JourneyReleaseTarget {
            project_url: project_url.as_str(),
            system_url: &inputs.system_pg_url,
            publisher: &publisher,
            project: project.as_ref(),
            control: admin.as_ref(),
            release_id: 3,
            attachments,
        },
    )
    .await?;
    let mut expected = previous.manifest().attachments.clone();
    for selected in selected_attachments {
        expected
            .get_mut(selected.id)
            .context("original caller proof attachment missing")?
            .auth_policy = auth_policy.clone();
    }
    anyhow::ensure!(
        release.manifest().format_version == 1
            && release.release().effective_release_id == 3
            && release.manifest().attachments == expected
            && release.manifest().components == previous.manifest().components
            && release.manifest().wirings == previous.manifest().wirings
            && release.manifest().registrations == previous.manifest().registrations,
        "caller proof changed facts beyond its release ID and selected route policies"
    );
    let after: Vec<u8> = project
        .query_one(
            "SELECT canonical_bytes FROM catalog.release_manifest_v3_snapshots \
         WHERE tenant_id = $1 AND effective_release_id = 2",
            &[&TENANT],
        )
        .await?
        .get(0);
    anyhow::ensure!(
        after == deployed_bytes,
        "nested fixture changed the deployed release 2"
    );

    let human = create_human(
        admin.as_ref(),
        "session-nested@example.test",
        "Nested session proof",
    )
    .await?;
    project_env_membership::grant(ProjectEnvMembershipArgs {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        principal_id: human.id().to_string(),
        system_database_url: inputs.system_pg_url.clone(),
    })
    .await?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
            &[&TENANT, &ROLE],
        )
        .await?;
    let mut permitted_operations = vec![OVERLAY_RECORD_RECEIPT, BASE_RECORD_RECEIPT];
    if client_proof {
        permitted_operations.push(OPERATION);
    }
    for operation in permitted_operations {
        project.execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, $2, $3)",
            &[&TENANT, &ROLE, &operation],
        ).await?;
    }
    project
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, email, status) \
         VALUES ($1, $2::text::uuid, 'session-nested@example.test', 'active')",
            &[&TENANT, &human.id().as_str()],
        )
        .await?;
    project.execute(
        "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ($1, $2::text::uuid, $3)",
        &[&TENANT, &human.id().as_str(), &ROLE],
    ).await?;
    let pat = issue_pat(
        admin.as_ref(),
        human.id(),
        "nested session proof",
        Duration::from_secs(600),
    )
    .await?;
    let client_login = if client_proof {
        Some(session_client::login(http.clone(), &endpoint, &audience, pat.token()).await?)
    } else {
        None
    };
    let (token, exchange) = if let Some(login) = &client_login {
        (login.credentials.bearer().await?, None)
    } else {
        let mut response = http
            .post(endpoint.join("/session")?)
            .bearer_auth(pat.token())
            .json(&serde_json::json!({"aud": audience}))
            .send()
            .await
            .context("exchange nested caller PAT at the real identity service")?;
        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "nested session exchange refused"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                chunk.len() <= 65_536 - body.len(),
                "nested session response exceeds bound"
            );
            body.extend_from_slice(&chunk);
        }
        let exchange: Value = serde_json::from_slice(&body)
            .map_err(|_| anyhow::anyhow!("nested session response is not JSON"))?;
        let token = exchange["access_token"]
            .as_str()
            .context("nested session response omitted token")?
            .to_owned();
        (token, Some(exchange))
    };
    let token = token.as_str();
    let verifier = SessionVerifier::new(keys, ORG, &audience)?;
    let verified = verifier.verify(token).await?;
    anyhow::ensure!(
        verified.claims().sub == human.id().as_str()
            && verified.claims().roles == [ROLE]
            && exchange
                .as_ref()
                .is_none_or(|exchange| exchange["token_type"] == "Bearer"
                    && exchange["expires_at"].as_i64() == Some(verified.claims().exp)),
        "real issuer changed the nested caller's signed identity"
    );
    let secret = |name: &str| {
        secret_value(
            &inputs.host_secret_directory.join(format!("{name}.json")),
            "url",
        )
    };
    let credentials = JourneyCredentials {
        guest_sql: secret("guest-sql")?,
        executor_platform: secret("executor-platform")?,
        event_materializer: secret("event-materializer")?,
        http_admitter: secret("http-admitter")?,
        identity_reader: secret("identity-reader")?,
        // This continuation does not start an authoring Gate or provision roles.
        control_author: String::new(),
        management_admitter: String::new(),
    };
    let traces = TraceHarness::install();
    let (engine, flow_http, routing, bridge, identity_task) =
        build_journey_runtime(&inputs, &credentials, release, Some(verifier.clone())).await?;
    // The base replays its stored result; the overlay reads the current Acme fields.
    // The PAT journey updated those fields after it first recorded this command.
    let expected_replay = project
        .query_one(
            "SELECT jsonb_build_object(\
           'receipt_id', command.receipt_id::text, \
           'purchase_order_id', command.purchase_order_id::text, \
           'purchase_order_status', command.purchase_order_status, \
           'row_version', command.row_version::text, \
           'acme_inspection_required', purchase.acme_inspection_required, \
           'acme_quality_status', purchase.acme_quality_status), purchase.row_version \
         FROM receiving.record_receipt_command AS command \
         JOIN receiving.purchase_order AS purchase ON purchase.id = command.purchase_order_id \
         WHERE command.idempotency_key = 'receipt-command-2' \
           AND purchase.id = '00000000-0000-0000-0000-000000000302'",
            &[],
        )
        .await?;
    let current_version: i64 = expected_replay.get(1);
    let expected_replay: Value = expected_replay.get(0);
    anyhow::ensure!(
        current_version == 3
            && expected_replay["purchase_order_status"] == "complete"
            && expected_replay["row_version"] == "2"
            && expected_replay["acme_inspection_required"] == true
            && expected_replay["acme_quality_status"] == "pending",
        "nested replay requires the completed PAT journey state"
    );
    let before = nested_receipt_state(project.as_ref()).await?;
    let request_body = Bytes::from_static(br#"[{"request_id":"session-nested-replay","value":{"idempotency_key":"receipt-command-2","purchase_order_id":"00000000-0000-0000-0000-000000000302","receipt_reference":"RECEIPT-2","occurred_at":"2026-08-31T12:31:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000502","quantity":"7.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#);
    if let Some(login) = client_login {
        let expected_base = serde_json::json!({
            "receipt_id": expected_replay["receipt_id"],
            "purchase_order_id": expected_replay["purchase_order_id"],
            "purchase_order_status": expected_replay["purchase_order_status"],
            "row_version": expected_replay["row_version"],
        });
        let transport = session_client::RouteTransport::new(engine, flow_http, routing, bridge, 61);
        let result = session_client::prove(
            fresh_only::Proof {
                inputs: &inputs,
                credentials: &credentials,
                project_url: project_url.as_str(),
                project: project.as_ref(),
                control: admin.as_ref(),
                publisher: &publisher,
                verifier,
                session: token,
                pat: pat.token(),
                body: request_body,
                expected_base: &expected_base,
                human_id: human.id().as_str(),
                traces: &traces,
                client_credentials: Some(login.credentials.clone()),
            },
            &login,
            transport,
            &human,
            &digests[BASE_PACKAGE_ID],
            direct_attachment.path,
        )
        .await;
        identity_task.abort();
        project_task.abort();
        admin_task.abort();
        result?;
        println!(
            "HOST_SESSION_CLIENT result=pass login=pass ordinary=pass fresh=pass nested=pass prior_commit=pass expired_pat=refused revoked_pat=refused"
        );
        return Ok(());
    }
    let (trace_id, traceparent) = journey_trace(31);
    // The existing command keeps the two-host GET fixture and event set unchanged.
    // A fresh-only refusal must stop before the base guest runs.
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("receiving_record_receipt"),
        Some(token),
        &traceparent,
        request_body.clone(),
    )
    .await?;
    if fresh_only {
        assert_operation_refusal(&response, "fresh-credential-required", BASE_RECORD_RECEIPT)?;
        let spans = traces.spans();
        assert_nested_permission_denial_trace(
            &spans,
            &trace_id,
            &digests[OVERLAY_PACKAGE_ID],
            &digests[BASE_PACKAGE_ID],
            human.id().as_str(),
        );
        let invoked = trace_component_invocations(&spans, &trace_id);
        anyhow::ensure!(
            span_attribute(invoked[0], "wamn.caller_credential_kind").as_deref() == Some("session"),
            "nested fresh-only refusal changed the originating credential"
        );
        anyhow::ensure!(
            nested_receipt_state(project.as_ref()).await? == before,
            "session refusal changed committed Receipt state"
        );

        // This is an explicit new request from the same human, not a host retry.
        let (pat_trace, pat_parent) = journey_trace(32);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &inputs.route_host,
            overlay_route_path("receiving_record_receipt"),
            Some(pat.token()),
            &pat_parent,
            request_body.clone(),
        )
        .await?;
        anyhow::ensure!(
            successful_value(&response, "session-nested-replay")? == expected_replay,
            "the same human's valid PAT failed the fresh-only operation"
        );
        assert_nested_record_receipt_trace(
            &traces.spans(),
            &pat_trace,
            &digests[OVERLAY_PACKAGE_ID],
            &digests[BASE_PACKAGE_ID],
            human.id().as_str(),
            "pat",
        );
        anyhow::ensure!(
            nested_receipt_state(project.as_ref()).await? == before,
            "the explicit PAT replay changed the original committed result"
        );

        let (direct_session_trace, direct_session_parent) = journey_trace(35);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &inputs.route_host,
            direct_attachment.path,
            Some(token),
            &direct_session_parent,
            request_body.clone(),
        )
        .await?;
        assert_operation_refusal(&response, "fresh-credential-required", BASE_RECORD_RECEIPT)?;
        assert_no_component_trace(&traces.spans(), &direct_session_trace);
        anyhow::ensure!(
            nested_receipt_state(project.as_ref()).await? == before,
            "direct session refusal changed committed Receipt state"
        );

        let (direct_pat_trace, direct_pat_parent) = journey_trace(36);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &inputs.route_host,
            direct_attachment.path,
            Some(pat.token()),
            &direct_pat_parent,
            request_body.clone(),
        )
        .await?;
        let expected_base = serde_json::json!({
            "receipt_id": expected_replay["receipt_id"],
            "purchase_order_id": expected_replay["purchase_order_id"],
            "purchase_order_status": expected_replay["purchase_order_status"],
            "row_version": expected_replay["row_version"],
        });
        anyhow::ensure!(
            successful_value(&response, "session-nested-replay")? == expected_base,
            "the same human's PAT failed the direct fresh-only operation"
        );
        let direct_spans = traces.spans();
        assert_direct_route_trace(
            &direct_spans,
            &direct_pat_trace,
            direct_attachment.wiring_id,
            BASE_RECORD_RECEIPT,
            &digests[BASE_PACKAGE_ID],
            human.id().as_str(),
        );
        let direct_invocations = trace_component_invocations(&direct_spans, &direct_pat_trace);
        anyhow::ensure!(
            span_attribute(direct_invocations[0], "wamn.caller_credential_kind").as_deref()
                == Some("pat"),
            "the direct fresh-only operation lost the originating PAT kind"
        );
        anyhow::ensure!(
            nested_receipt_state(project.as_ref()).await? == before,
            "the direct PAT replay changed the original committed result"
        );

        fresh_only::prove_prior_commit(fresh_only::Proof {
            inputs: &inputs,
            credentials: &credentials,
            project_url: project_url.as_str(),
            project: project.as_ref(),
            control: admin.as_ref(),
            publisher: &publisher,
            verifier: verifier.clone(),
            session: token,
            pat: pat.token(),
            body: request_body.clone(),
            expected_base: &expected_base,
            human_id: human.id().as_str(),
            traces: &traces,
            client_credentials: None,
        })
        .await?;

        let removed = project
            .execute(
                "DELETE FROM app_system.user_roles \
             WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3",
                &[&TENANT, &human.id().as_str(), &ROLE],
            )
            .await?;
        anyhow::ensure!(
            removed == 1,
            "role removal must remove the human's one assignment"
        );
        let (role_trace, role_parent) = journey_trace(33);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &inputs.route_host,
            overlay_route_path("receiving_record_receipt"),
            Some(pat.token()),
            &role_parent,
            request_body.clone(),
        )
        .await?;
        assert_operation_refusal(&response, "permission-denied", OVERLAY_RECORD_RECEIPT)?;
        assert_no_component_trace(&traces.spans(), &role_trace);
        project
            .execute(
                "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
             VALUES ($1, $2::text::uuid, $3)",
                &[&TENANT, &human.id().as_str(), &ROLE],
            )
            .await?;
        project_env_membership::revoke(ProjectEnvMembershipArgs {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            env: ENVIRONMENT.to_owned(),
            principal_id: human.id().to_string(),
            system_database_url: inputs.system_pg_url.clone(),
        })
        .await?;
        let (membership_trace, membership_parent) = journey_trace(34);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &inputs.route_host,
            overlay_route_path("receiving_record_receipt"),
            Some(pat.token()),
            &membership_parent,
            request_body,
        )
        .await?;
        anyhow::ensure!(
            response.status() == StatusCode::UNAUTHORIZED
                && response.body().as_ref() == br#"{"error":{"code":"unauthorized"}}"#,
            "the next PAT request did not refuse revoked environment membership"
        );
        assert_no_component_trace(&traces.spans(), &membership_trace);
        anyhow::ensure!(
            nested_receipt_state(project.as_ref()).await? == before,
            "revocation refusals changed committed Receipt state"
        );
    } else {
        let value = successful_value(&response, "session-nested-replay")?;
        anyhow::ensure!(
            value == expected_replay,
            "nested session replay returned the wrong operation result"
        );
        assert_nested_record_receipt_trace(
            &traces.spans(),
            &trace_id,
            &digests[OVERLAY_PACKAGE_ID],
            &digests[BASE_PACKAGE_ID],
            human.id().as_str(),
            "session",
        );
    }
    identity_task.abort();
    project_task.abort();
    admin_task.abort();
    if fresh_only {
        println!(
            "HOST_FRESH_ONLY_NESTED result=pass session=refused pat=accepted direct_session=refused direct_pat=accepted role_revocation=refused membership_revocation=refused committed_state=unchanged fixture_release=3 manifest_format=1"
        );
    } else {
        println!(
            "HOST_SESSION_NESTED result=pass credential_kind=session invocations=2 fixture_release=3 manifest_format=1"
        );
    }
    Ok(())
}

pub(super) fn assert_operation_refusal(
    response: &hyper::Response<Bytes>,
    code: &str,
    operation: &str,
) -> anyhow::Result<()> {
    let body: Value = serde_json::from_slice(response.body())?;
    anyhow::ensure!(
        response.status() == StatusCode::FORBIDDEN
            && body == serde_json::json!({"error": {"code": code, "operation": operation}}),
        "operation refusal differs from its exact HTTP contract: status={} body={body}",
        response.status()
    );
    Ok(())
}

pub(super) async fn nested_receipt_state(project: &Client) -> anyhow::Result<Value> {
    Ok(project.query_one(
        "SELECT jsonb_build_object(\
           'commands', (SELECT jsonb_agg(to_jsonb(row) ORDER BY idempotency_key) \
             FROM receiving.record_receipt_command AS row), \
           'receipts', (SELECT jsonb_agg(to_jsonb(row) ORDER BY id) FROM receiving.receipt AS row), \
           'receipt_lines', (SELECT jsonb_agg(to_jsonb(row) ORDER BY id) FROM receiving.receipt_line AS row), \
           'orders', (SELECT jsonb_agg(to_jsonb(row) ORDER BY id) FROM receiving.purchase_order AS row), \
           'order_lines', (SELECT jsonb_agg(to_jsonb(row) ORDER BY id) FROM receiving.purchase_order_line AS row))",
        &[],
    ).await.context("read committed Receipt state independently")?.get(0))
}
