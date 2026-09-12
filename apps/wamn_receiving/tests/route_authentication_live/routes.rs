//! Receiving routes tests and helpers.

use super::*;


#[tokio::test]
#[ignore = "requires disposable PG18 and authenticated OCI plus built virtualized base, overlay, and flow-http artifacts"]
async fn production_two_package_release_serves_all_thirteen_pat_routes() -> anyhow::Result<()> {
    receiving_pat_journey(&JourneyDocument::required()?, &journey_scenario_worker_binary()?, false).await.map(|_| ())
}

#[tokio::test]
#[ignore = "requires the dedicated fresh-only disposable journey and copied package directory"]
async fn production_two_package_fresh_only_fixture_serves_all_thirteen_pat_routes()
-> anyhow::Result<()> {
    receiving_pat_journey(&JourneyDocument::required()?, &journey_scenario_worker_binary()?, true).await.map(|_| ())
}

pub(super) fn copy_fresh_only_package(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_fresh_only_package(&entry.path(), &target)?;
        } else {
            anyhow::ensure!(
                kind.is_file(),
                "fresh-only package sources must be regular files"
            );
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub(super) fn prepare_fresh_only_packages(root: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!root.exists(), "fresh-only package directory must be new");
    std::fs::create_dir(root)?;
    // Keep the app directory names when copying their generated output.
    for source in [package_root(), overlay_package_root()] {
        let directory = source
            .file_name()
            .expect("the proof package has a directory name");
        copy_fresh_only_package(&source, &root.join(directory))?;
    }
    let base = root.join("wamn_receiving");
    let manifest_path = base.join("wamn.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    let operation = manifest["custom_operations"]
        .get_mut("receiving.record_receipt")
        .context("the base manifest must declare record_receipt")?;
    operation["fresh_only"] = Value::Bool(true);
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let declaration_path = base.join("publication/components/receiving.json.in");
    let mut declaration: Value = serde_json::from_slice(&std::fs::read(&declaration_path)?)?;
    declaration["operations"]
        .get_mut(BASE_RECORD_RECEIPT)
        .context("the base declaration must name the registered operation")?["fresh-only"] =
        Value::Bool(true);
    std::fs::write(declaration_path, serde_json::to_vec(&declaration)?)?;
    Ok(())
}

#[test]
fn fresh_only_fixture_changes_copies_without_changing_business_policy() -> anyhow::Result<()> {
    let source_manifest = package_root().join("wamn.json");
    let source_declaration = package_root().join("publication/components/receiving.json.in");
    let manifest_before = std::fs::read(&source_manifest)?;
    let declaration_before = std::fs::read(&source_declaration)?;
    let scratch = ScratchRoot::create()?;
    let copies = scratch.path().join("packages");
    prepare_fresh_only_packages(&copies)?;
    let base = copies.join("wamn_receiving");
    for (directory, generated) in [
        ("wamn_receiving", "receiving"),
        ("client_acme_receiving", "client_acme_receiving"),
    ] {
        assert!(
            copies
                .join(directory)
                .join(format!("generated/{generated}-tui/Cargo.toml"))
                .is_file(),
            "copied TUI output must retain its generated directory name"
        );
    }
    let manifest: Value = serde_json::from_slice(&std::fs::read(base.join("wamn.json"))?)?;
    let declaration: Value = serde_json::from_slice(&std::fs::read(
        base.join("publication/components/receiving.json.in"),
    )?)?;
    assert_eq!(
        manifest["custom_operations"]["receiving.record_receipt"]["fresh_only"],
        true
    );
    assert_eq!(
        declaration["operations"][BASE_RECORD_RECEIPT]["fresh-only"],
        true
    );
    assert_eq!(std::fs::read(source_manifest)?, manifest_before);
    assert_eq!(std::fs::read(source_declaration)?, declaration_before);
    assert!(
        prepare_fresh_only_packages(&copies).is_err(),
        "an existing fixture is never overwritten"
    );
    Ok(())
}

pub(super) async fn receiving_pat_journey(
    inputs: &JourneyDocument,
    scenario_worker: &Path,
    fresh_only: bool,
) -> anyhow::Result<wamn_ctl::dev::environment::ProvisionedRoute> {
    anyhow::ensure!(
        inputs.fresh_only_packages.is_some() == fresh_only,
        "the selected journey must match its fresh-only package fixture"
    );
    if let Some(root) = &inputs.fresh_only_packages {
        prepare_fresh_only_packages(root)?;
    }
    if let Some(phase) = &inputs.overlay_compatibility {
        anyhow::ensure!(
            !fresh_only,
            "overlay compatibility cannot combine with fresh-only"
        );
        overlay_compatibility::prepare(phase)?;
    }
    let system_url = inputs.system_pg_url.clone();
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let (admin, admin_task) = connect(&system_url).await?;
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .context("read PostgreSQL version")?
        .get::<_, String>(0)
        .parse()
        .context("parse PostgreSQL version")?;
    anyhow::ensure!(
        version >= 180_000,
        "journey requires PostgreSQL 18 or newer"
    );

    provision_journey_control(&system_url, admin.as_ref()).await?;
    let management_secret = root.join("management-author-pat.json");
    let route =
        provision_route(&system_url, admin.as_ref(), root, Some(&management_secret)).await?;
    let caller_principal_id = resolve_subject(
        admin.as_ref(),
        PrincipalKind::Service,
        &route.principal_subject,
    )
    .await
    .context("resolve the production route-caller principal")?
    .context("the production route-caller principal is absent")?
    .id()
    .to_string();
    let route_caller_secret = root.join("route-caller-pat.json");
    std::fs::copy(&route_caller_secret, &inputs.route_caller_secret_output).with_context(|| {
        format!(
            "copy production-minted route-caller Secret from {} to {}",
            route_caller_secret.display(),
            inputs.route_caller_secret_output.display()
        )
    })?;
    std::fs::set_permissions(
        &inputs.route_caller_secret_output,
        Permissions::from_mode(0o600),
    )
    .with_context(|| {
        format!(
            "set route-caller Secret mode on {}",
            inputs.route_caller_secret_output.display()
        )
    })?;
    let (project, project_task) = connect(&route.database_url).await?;
    install_journey_project(inputs, project.as_ref(), &route.database_url, fresh_only).await?;
    verify_journey_operation_grants(project.as_ref()).await?;
    reconcile_journey_run_plane(&system_url, &route.database_url).await?;
    let credentials = prepare_journey_credentials(
        &system_url,
        &route.database_url,
        root,
        &inputs.host_secret_directory,
        &inputs.host_secret_namespace,
    )
    .await?;
    if let Some(copies) = &inputs.fresh_only_packages {
        for name in ["control-author", "management-admitter"] {
            let destination = copies.join(format!("{name}.json"));
            anyhow::ensure!(
                !destination.exists(),
                "fresh-only authority copy must be new"
            );
            std::fs::copy(root.join(format!("{name}.json")), &destination)?;
            std::fs::set_permissions(&destination, Permissions::from_mode(0o600))?;
        }
    }
    reconcile_journey_data_access(inputs, &route.database_url).await?;
    let declarations = render_component_declarations(Some(inputs), root, &inputs.component_directory)?;
    push_journey_components(&inputs, &route.database_url, &system_url, &declarations).await?;
    let admitted_component_digests =
        verify_journey_components_are_effectful(project.as_ref()).await?;

    // One gate-launch path for both live gates: the Gate is a spawned
    // `wamn-scenario-worker serve`, never a task in this process
    // (wamn-10yt.10.32). The in-process exemption this test used to hold died
    // with that ruling.
    let mut management_server = spawn_journey_management_gate(
        scenario_worker,
        &credentials,
        &credentials.management_admitter,
        if inputs.host_secret_namespace == "wamn-receiving-correctness" {
            "127.0.0.1:18090"
        } else {
            ROUTE_JOURNEY_GATE_BIND
        },
    )
    .await?;
    let gate_reports = gate_journey_wirings(
        inputs,
        management_server.bind(),
        route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
    )
    .await?;
    verify_zero_case_gate_reports(admin.as_ref(), &gate_reports).await?;
    author_journey_wirings(inputs, &route.database_url, &system_url).await?;
    reconcile_journey_run_plane(&system_url, &route.database_url).await?;
    let (_, release) = publish_journey_release(
        &inputs,
        JourneyReleaseTarget {
            project_url: &route.database_url,
            system_url: &system_url,
            publisher: route
                .management_principal_subject
                .as_deref()
                .context("project provisioning emitted no management-author principal")?,
            project: project.as_ref(),
            control: admin.as_ref(),
            release_id: RELEASE_ID,
            attachments: JOURNEY_PACKAGES
                .iter()
                .map(|package| journey_publication_root(*package, Some(inputs)).join("attachments.json"))
                .collect(),
        },
    )
    .await?;
    let component_digests = released_component_digests(&release, &inputs.route_host)?;
    anyhow::ensure!(
        component_digests == admitted_component_digests,
        "released component digests differ from the two admitted artifacts"
    );
    seed_receiving_business_rows(project.as_ref()).await?;

    let traces = TraceHarness::install();
    let (engine, flow_http, routing, bridge, identity_task) =
        build_journey_runtime(&inputs, &credentials, release, None).await?;
    let mut expected_direct_traces = Vec::new();

    let (cold_nested_trace, traceparent) = journey_trace(1);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("receiving_record_receipt"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-record-receipt","value":{"idempotency_key":"receipt-command-2","purchase_order_id":"00000000-0000-0000-0000-000000000302","receipt_reference":"RECEIPT-2","occurred_at":"2026-08-31T12:31:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000502","quantity":"7.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-record-receipt").with_context(|| {
        let failures: Vec<_> = traces
            .spans()
            .into_iter()
            .flat_map(|span| {
                span.events
                    .events
                    .into_iter()
                    .flat_map(|event| event.attributes)
                    .filter(|attribute| attribute.key.as_str() == "error")
                    .map(|attribute| attribute.value.to_string())
            })
            .collect();
        format!("cold Receiving route trace errors: {failures:?}")
    })?;
    let memory = response
        .extensions()
        .get::<JourneyGuestMemory>()
        .context("cold nested Receiving invocation did not report guest memory")?;
    anyhow::ensure!(
        memory.shell_bytes > 0 && memory.peak_bytes > memory.shell_bytes,
        "cold nested Receiving invocation did not share its budget with flow-http: {memory:?}"
    );
    anyhow::ensure!(
        value["purchase_order_status"] == "complete"
            && value["row_version"] == "2"
            && value["acme_inspection_required"] == false
            && value["acme_quality_status"] == "not_required",
        "cold Acme receiving.record_receipt returned the wrong result: {value}"
    );

    let (_, traceparent) = journey_trace(15);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/location/list",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(br#"[{"request_id":"location-list"}]"#),
    )
    .await?;
    let value = successful_value(&response, "location-list")?;
    anyhow::ensure!(
        value["rows"].as_array().is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["id"] == "00000000-0000-0000-0000-000000000201"
                && rows[0]["location_code"] == "DOCK-1"
        }),
        "location.list returned the wrong rows: {value}"
    );

    let (_, traceparent) = journey_trace(16);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receiving/load_receipt_screen",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"load-receipt-screen","purchase_order_id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "load-receipt-screen")?;
    anyhow::ensure!(
        value["rows"].as_array().is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["purchase_order_id"] == "00000000-0000-0000-0000-000000000301"
                && rows[0]["line_id"] == "00000000-0000-0000-0000-000000000501"
                && rows[0]["remaining_quantity"] == "5.0000"
        }),
        "receiving.load_receipt_screen returned the wrong rows: {value}"
    );

    let (trace_id, traceparent) = journey_trace(2);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-get","id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-get")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000301" && value["row_version"] == "1",
        "purchase_order.get returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_get",
        "wamn-receiving:purchase-order/get@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(3);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/query",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-query","filter":{"supplier_id":["00000000-0000-0000-0000-000000000401"],"status":["open"]},"sort":{"field":"created_at","direction":"ascending"},"limit":100}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-query")?;
    anyhow::ensure!(
        value["item"].as_array().is_some_and(|items| {
            items.len() == 1 && items[0]["id"] == "00000000-0000-0000-0000-000000000301"
        }),
        "purchase_order.query returned the wrong page: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_query",
        "wamn-receiving:purchase-order/query@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(4);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/update",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-update","id":"00000000-0000-0000-0000-000000000301","expected_row_version":"1","change":{"supplier_id":"00000000-0000-0000-0000-000000000402"}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-update")?;
    anyhow::ensure!(
        value["supplier_id"] == "00000000-0000-0000-0000-000000000402"
            && value["row_version"] == "2",
        "purchase_order.update returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_update",
        "wamn-receiving:purchase-order/update@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(5);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receiving/record_receipt",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"record-receipt","value":{"idempotency_key":"receipt-command-1","purchase_order_id":"00000000-0000-0000-0000-000000000301","receipt_reference":"RECEIPT-1","occurred_at":"2026-08-31T12:30:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000501","quantity":"5.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "record-receipt")?;
    anyhow::ensure!(
        value["purchase_order_status"] == "complete" && value["row_version"] == "3",
        "receiving.record_receipt returned the wrong command result: {value}"
    );
    let receipt_id = value["receipt_id"]
        .as_str()
        .context("record_receipt returned no receipt_id")?
        .to_owned();
    expected_direct_traces.push((
        trace_id,
        "receiving_record_receipt",
        BASE_RECORD_RECEIPT,
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(6);
    let receipt_get = serde_json::to_vec(&serde_json::json!([{
        "request_id": "receipt-get",
        "id": receipt_id,
    }]))?;
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receipt/get",
        Some(&route.token),
        &traceparent,
        Bytes::from(receipt_get),
    )
    .await?;
    let value = successful_value(&response, "receipt-get")?;
    anyhow::ensure!(
        value["receipt_reference"] == "RECEIPT-1",
        "receipt.get returned the wrong receipt: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "receipt_get",
        "wamn-receiving:receipt/get@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(7);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receipt/query",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(br#"[{"request_id":"receipt-query","limit":100}]"#),
    )
    .await?;
    let value = successful_value(&response, "receipt-query")?;
    anyhow::ensure!(
        value["item"].as_array().is_some_and(|items| {
            items.len() == 2
                && items
                    .iter()
                    .any(|item| item["receipt_reference"] == "RECEIPT-1")
                && items
                    .iter()
                    .any(|item| item["receipt_reference"] == "RECEIPT-2")
        }),
        "receipt.query returned the wrong page: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "receipt_query",
        "wamn-receiving:receipt/query@1.0.0",
        BASE_PACKAGE_ID,
    ));
    seed_preexisting_quality_fixture(project.as_ref()).await?;

    let (trace_id, traceparent) = journey_trace(8);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("purchase_order_get"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-purchase-order-get","id":"00000000-0000-0000-0000-000000000302"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-purchase-order-get")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000302"
            && value["row_version"] == "2"
            && value["acme_inspection_required"] == false
            && value["acme_quality_status"] == "not_required",
        "Acme purchase_order.get returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_get",
        "client-acme-receiving:purchase-order/get@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(9);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("purchase_order_update"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-purchase-order-update","id":"00000000-0000-0000-0000-000000000302","expected_row_version":"2","change":{"acme_inspection_required":true,"acme_quality_status":"pending"}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-purchase-order-update")?;
    anyhow::ensure!(
        value["row_version"] == "3"
            && value["acme_inspection_required"] == true
            && value["acme_quality_status"] == "pending",
        "Acme purchase_order.update returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_update",
        "client-acme-receiving:purchase-order/update@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(10);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("quality_load_purchase_order_detail"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"quality-load-detail","purchase_order_id":"00000000-0000-0000-0000-000000000302"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "quality-load-detail")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000302"
            && value["row_version"] == "3"
            && value["acme_quality_status"] == "pending",
        "quality.load_purchase_order_detail returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "quality_load_purchase_order_detail",
        "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(11);
    let approve = serde_json::to_vec(&serde_json::json!([{
        "request_id": "quality-approve",
        "receipt_id": PREEXISTING_QUALITY_RECEIPT_ID,
        "expected_row_version": "1",
    }]))?;
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("quality_approve_inspection"),
        Some(&route.token),
        &traceparent,
        Bytes::from(approve),
    )
    .await?;
    let value = successful_value(&response, "quality-approve")?;
    anyhow::ensure!(
        value["status"] == "approved"
            && value["row_version"] == "2"
            && value["purchase_order_id"] == "00000000-0000-0000-0000-000000000303"
            && value["purchase_order_row_version"] == "3",
        "quality.approve_inspection returned the wrong result: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "quality_approve_inspection",
        "client-acme-receiving:quality/approve-inspection@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let removed = project
        .execute(
            "DELETE FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 AND permission = $3",
            &[&TENANT, &ROUTE_CALLER_ROLE, &BASE_RECORD_RECEIPT],
        )
        .await
        .context("remove only the pinned-base record_receipt permission")?;
    anyhow::ensure!(
        removed == 1,
        "nested-denial setup removed {removed} permission rows instead of one"
    );
    let (denied_nested_trace, denied_nested_parent) = journey_trace(12);
    let denied_nested = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("receiving_record_receipt"),
        Some(&route.token),
        &denied_nested_parent,
        Bytes::from_static(
            br#"[{"request_id":"nested-permission-denied","value":{"idempotency_key":"receipt-command-denied","purchase_order_id":"00000000-0000-0000-0000-000000000302","receipt_reference":"RECEIPT-DENIED","occurred_at":"2026-08-31T12:32:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000502","quantity":"1.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let denied_nested_body: Value = serde_json::from_slice(denied_nested.body())
        .context("decode the nested permission refusal")?;
    anyhow::ensure!(
        denied_nested.status() == StatusCode::FORBIDDEN
            && denied_nested_body
                == serde_json::json!({
                    "error": {
                        "code": "permission-denied",
                        "operation": BASE_RECORD_RECEIPT,
                    }
                }),
        "nested permission refusal was not the exact discoverable 403 contract: status={} body={denied_nested_body}",
        denied_nested.status()
    );

    let (unauthorized_trace, unauthorized_parent) = journey_trace(13);
    let unauthorized = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        None,
        &unauthorized_parent,
        Bytes::from_static(
            br#"[{"request_id":"unauthorized","id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    anyhow::ensure!(
        unauthorized.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated Receiving route returned {}: {}",
        unauthorized.status(),
        String::from_utf8_lossy(unauthorized.body())
    );

    let (oversized_trace, oversized_parent) = journey_trace(14);
    let oversized = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        Some(&route.token),
        &oversized_parent,
        Bytes::from(vec![b' '; RAW_BODY_LIMIT + 1]),
    )
    .await?;
    anyhow::ensure!(
        oversized.status() == StatusCode::PAYLOAD_TOO_LARGE
            && oversized
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .is_some_and(|value| value == "text/plain; charset=utf-8")
            && oversized.body().as_ref() == b"request body exceeds 1048576-byte limit\n",
        "oversized Receiving route returned {}: {}",
        oversized.status(),
        String::from_utf8_lossy(oversized.body())
    );

    p3_shell::assert_p3_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        &route.token,
    )
    .await?;

    let spans = traces.spans();
    for (trace_id, wiring_id, operation, package_id) in expected_direct_traces {
        let component_digest = component_digests
            .get(package_id)
            .with_context(|| format!("released {package_id} component digest is missing"))?;
        assert_direct_route_trace(
            &spans,
            &trace_id,
            wiring_id,
            operation,
            component_digest,
            &caller_principal_id,
        );
    }
    let overlay_digest = component_digests
        .get(OVERLAY_PACKAGE_ID)
        .context("released overlay component digest is missing")?;
    let base_digest = component_digests
        .get(BASE_PACKAGE_ID)
        .context("released base component digest is missing")?;
    assert_nested_record_receipt_trace(
        &spans,
        &cold_nested_trace,
        overlay_digest,
        base_digest,
        &caller_principal_id,
        "pat",
    );
    assert_native_nested_acquisition(&spans, &cold_nested_trace, overlay_digest, base_digest);
    assert_nested_permission_denial_trace(
        &spans,
        &denied_nested_trace,
        overlay_digest,
        base_digest,
        &caller_principal_id,
    );
    assert_no_component_trace(&spans, &unauthorized_trace);
    assert_no_component_trace(&spans, &oversized_trace);

    // The denial arm mutates one operation grant deliberately. Its package is
    // the author of that grant, so reapply the exact coordinate before handing
    // this disposable release to the operator-managed materializer continuation.
    let base_package = JOURNEY_PACKAGES
        .into_iter()
        .find(|package| package.id == BASE_PACKAGE_ID)
        .context("find the base package in the journey release")?;
    apply_package::run(ApplyPackageArgs {
        package: journey_package_root(base_package, Some(inputs)),
        database_url: route.database_url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("restore the base package's exact operation grants")?;
    reconcile_journey_data_access(inputs, &route.database_url).await?;
    verify_journey_operation_grants(project.as_ref()).await?;
    seed_materializer_trigger_rows(project.as_ref()).await?;
    if let Some(phase) = &inputs.overlay_compatibility {
        overlay_compatibility::after_install(
            phase,
            &route.database_url,
            &inputs.component_directory,
        )
        .await?;
        if phase.base == BaseCandidate::Baseline {
            overlay_compatibility::breaking_refusal(
                phase,
                &system_url,
                &inputs.component_directory,
            )
            .await?;
        }
    }

    let gate_stop = management_server.shutdown().await;
    identity_task.abort();
    project_task.abort();
    admin_task.abort();
    gate_stop?;
    Ok(route)
}
