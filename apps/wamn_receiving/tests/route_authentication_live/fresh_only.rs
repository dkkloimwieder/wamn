//! A parent composed with the released fresh-only base carries the base's grant.
//!
//! Publish folds the base's permission and fresh_only into the parent's entry,
//! so a caller without the base permission is refused at activation, before the
//! parent's own SQL runs. With the permission, one invocation runs the parent's
//! SQL and the embedded base.

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_catalog::ComponentDeclaration;

use super::{
    BASE_PACKAGE_ID, BASE_PACKAGE_VERSION, BASE_RECORD_RECEIPT, JOURNEY_PACKAGES, identity,
    journey_publication_root,
};

const PACKAGE: &str = "fresh_only_probe";
const VERSION: &str = "2.0.0";
const OPERATION: &str = "wamn:node/async-handler@0.1.0";
const WIRING: &str = "prior_commit";
const FOREIGN_TENANT: &str = "prior-commit-other-tenant";
const COUNTER_SQL: &str = "UPDATE fresh_only_probe.counter SET count = count + 1";
const COUNTER_POLICY_SQL: &str = "CREATE POLICY prior_commit_tenant ON fresh_only_probe.counter \
    TO wamn_app \
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) \
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())";

fn fixture_manifest(base_digest: &str) -> Value {
    json!({
        "package": {"id": PACKAGE, "version": VERSION},
        "base_dependencies": {"base_receiving": {
            "package": BASE_PACKAGE_ID, "version": *BASE_PACKAGE_VERSION,
            "digest": base_digest, "operations": ["receiving.record_receipt"]
        }},
        "required_platform_policy_contract": {"id": "prior_commit_fixture", "state": "satisfied"},
        "models": {"counter": {
            "schema": PACKAGE, "table": "counter", "owner": PACKAGE,
            "audit_log": {"columns": [], "retention": "none"},
            "operations": {"get": {
                "permission": "counter.get", "result": "one"}}
        }},
        "connections": ["postgres"],
        "components": {(WIRING): {"connections": ["postgres"]}}
    })
}

fn parent_component() -> anyhow::Result<Vec<u8>> {
    let path = std::env::var_os("WAMN_PRIOR_COMMIT_COMPONENT")
        .context("WAMN_PRIOR_COMMIT_COMPONENT must name the virtualized fixture")?;
    std::fs::read(&path).with_context(|| {
        format!(
            "read WAMN_PRIOR_COMMIT_COMPONENT {}",
            std::path::Path::new(&path).display()
        )
    })
}

/// The built application components.
fn application_components() -> anyhow::Result<std::path::PathBuf> {
    std::env::var_os("WAMN_APPLICATION_COMPONENTS")
        .map(Into::into)
        .context("WAMN_APPLICATION_COMPONENTS must name the built application components")
}

/// The built Receiving base, which the parent embeds.
fn built_base() -> anyhow::Result<Vec<u8>> {
    let path = application_components()?.join("receiving.wasm");
    std::fs::read(&path).with_context(|| format!("read built base {}", path.display()))
}

/// The base's generated no-op participant. The parent names no participant, so
/// composition plugs it into the base's optional record_receipt slot.
fn built_no_op(directory: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    let path = directory.join("wamn_receiving_receiving_record_receipt_no_op.wasm");
    std::fs::read(&path).with_context(|| format!("read built no-op participant {}", path.display()))
}

#[test]
#[ignore = "requires: WAMN_PRIOR_COMMIT_COMPONENT, WAMN_APPLICATION_COMPONENTS"]
fn counter_parent_has_the_real_node_and_nested_operation_abi() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_PRIOR_COMMIT_COMPONENT",
        "WAMN_APPLICATION_COMPONENTS",
    ]);
    let base = built_base()?;
    let base_digest = wamn_engine::component_admission::component_digest(&base);
    let manifest = wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(
        &fixture_manifest(&base_digest),
    )?)?;
    let operations = wamn_schema_generator::validate_operation_vocabulary(&manifest)?;
    anyhow::ensure!(
        operations == ["counter.get".to_owned()].into(),
        "fixture must declare exactly the generated counter read it exercises"
    );
    let engine = wamn_engine::engine::build_engine(&[])?;
    let declaration = json!({
        "scope": {"tenant-id": "fixture", "package-id": PACKAGE, "package-version": VERSION},
        "component": WIRING, "interface-version": "0.1.0", "connections": [],
        "operations": {(OPERATION): {"dependencies": [{"package": BASE_PACKAGE_ID,
            "version": *BASE_PACKAGE_VERSION, "digest": base_digest,
            "operation": *BASE_RECORD_RECEIPT}],
            "input-ports": [{"name": "input", "schema": {"type": "array"}}],
            "output-ports": [{"name": "main", "schema": {"type": "array"}}], "parameters": []}}
    });
    let base_declaration = wamn_schema_generator::operation_reference::read_authored_document(
        &journey_publication_root(
            *JOURNEY_PACKAGES
                .iter()
                .find(|package| package.id == BASE_PACKAGE_ID)
                .context("the journey must declare the actual base package")?,
            None,
        )
        .join("components/receiving.json.in"),
        wamn_schema_generator::operation_reference::AuthoredDocument::Declaration,
    )?;
    // Composition type-checks the parent's import against the base's export.
    let composed = wamn_component_composer::compose(
        &declaration,
        parent_component()?,
        vec![wamn_component_composer::Base {
            declaration: base_declaration,
            bytes: base,
        }],
        Vec::new(),
        &[built_no_op(&application_components()?)?],
    )?;
    let declaration: ComponentDeclaration = serde_json::from_value(declaration)?;
    let admitted = wamn_engine::component_admission::validate_component_admission(
        &engine,
        &composed,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: ["wamn:node".to_owned(), "wamn:postgres".to_owned()].into(),
            effect_free_operation_dependencies: std::collections::BTreeSet::default(),
        },
    )?;
    anyhow::ensure!(
        !admitted.component.operations[OPERATION].fresh_only,
        "instrumentation parent must remain ordinary"
    );
    Ok(())
}

#[cfg(test)]
mod execution_tests {
    use super::{BASE_RECORD_RECEIPT, COUNTER_SQL, OPERATION, parent_component};
    use anyhow::Context as _;
    use wash_runtime::wasmtime::component::{Component, Linker, ResourceTable, TypedFunc};
    use wash_runtime::wasmtime::{Engine, Store};
    use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

    mod bindings {
        wash_runtime::wasmtime::component::bindgen!({
            world: "fixture:prior-commit/host",
            inline: r#"
                package fixture:prior-commit;

                world host {
                  import wamn:postgres/client@0.3.0;
                  import wamn-receiving:receiving/record-receipt@2.1.0;
                  export wamn:node/async-handler@0.1.0;
                }
            "#,
            path: [
                "../../../crates/execution/workflow/router/wit",
                "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.3",
                "../../../apps/target/wamn/wamn_receiving/wit/deps/wamn-receiving-receiving",
            ],
            additional_derives: [PartialEq],
            wasmtime_crate: wash_runtime::wasmtime,
        });
    }
    use bindings::wamn::node::types::{Emission, NodeContext, NodeError};
    use bindings::wamn::postgres::types::{PgError, SqlValue};
    use bindings::wamn_receiving::receiving::record_receipt::{
        RecordReceiptItem, RecordReceiptOutcome, RecordReceiptRequest, RecordReceiptResult,
    };

    struct Calls {
        order: Vec<&'static str>,
        nested: Option<(NodeContext, Vec<RecordReceiptItem>)>,
        table: ResourceTable,
        wasi: WasiCtx,
    }

    impl Default for Calls {
        fn default() -> Self {
            Self {
                order: Vec::new(),
                nested: None,
                table: ResourceTable::new(),
                wasi: WasiCtxBuilder::new().build(),
            }
        }
    }

    impl WasiView for Calls {
        fn ctx(&mut self) -> WasiCtxView<'_> {
            WasiCtxView {
                ctx: &mut self.wasi,
                table: &mut self.table,
            }
        }
    }

    #[tokio::test]
    async fn counter_uses_login_tenant_without_a_guest_guc() -> anyhow::Result<()> {
        use wamn_control_provision::sql::{
            ensure_app_acl_role_sql, ensure_db_owner_role_sql, prepare_workload_generation_sql,
        };
        use wamn_control_provision::tenant_key::authority_derivations_sql;
        use wamn_control_provision::workload_role::{
            WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
        };
        use wamn_run_state::CredentialGeneration;

        const DATABASE: &str = "wamn-db-acme--billing--prior-commit";
        // The test requires a server without the shared roles, so it starts its own.
        let server = wamn_test_infrastructure::postgres::start(&[])?;
        let config: tokio_postgres::Config = server
            .database("postgres")?
            .url()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid tenant-key test administrator URL"))?;
        anyhow::ensure!(
            config.get_dbname() == Some("postgres") && config.get_options().is_none(),
            "tenant-key test requires a fresh postgres database without session options"
        );
        let (admin, driver) = config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(|_| anyhow::anyhow!("connect tenant-key test administrator"))?;
        let admin_task = tokio::spawn(async move {
            let _ = driver.await;
        });
        let pristine: bool = admin
            .query_one(
                "SELECT \
            current_setting('server_version_num')::int / 10000 = 18 \
            AND (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) \
            AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_db_owner', 'wamn_app')) \
            AND NOT EXISTS (SELECT FROM pg_database WHERE datname = $1)",
                &[&DATABASE],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            pristine,
            "tenant-key test requires a fresh PostgreSQL 18 superuser server"
        );
        admin.batch_execute(ensure_db_owner_role_sql()).await?;
        admin.batch_execute(&ensure_app_acl_role_sql()).await?;
        admin
            .batch_execute(&format!(
                "CREATE DATABASE \"{DATABASE}\" OWNER wamn_db_owner"
            ))
            .await?;
        let role = workload_generation_role(
            WorkloadRoleFamily::App,
            WorkloadRoleScope::Tenant {
                tenant: super::identity().tenant.as_str(),
                database: DATABASE,
            },
            CredentialGeneration::A,
        )?;
        let password = uuid::Uuid::new_v4().to_string();
        admin
            .batch_execute(&prepare_workload_generation_sql(
                WorkloadRoleFamily::App,
                DATABASE,
                &role,
                &password,
                "infinity",
            ))
            .await?;
        let mut project_config = config.clone();
        project_config.dbname(DATABASE);
        let (project, driver) = project_config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(|_| anyhow::anyhow!("connect tenant-key test database"))?;
        let project_task = tokio::spawn(async move {
            let _ = driver.await;
        });
        project
            .batch_execute(&authority_derivations_sql(DATABASE))
            .await?;
        project.batch_execute("CREATE SCHEMA fresh_only_probe; \
            CREATE TABLE fresh_only_probe.counter (tenant_id text PRIMARY KEY, count bigint NOT NULL); \
            ALTER TABLE fresh_only_probe.counter ENABLE ROW LEVEL SECURITY; \
            ALTER TABLE fresh_only_probe.counter FORCE ROW LEVEL SECURITY; \
            CREATE POLICY prior_commit_tenant ON fresh_only_probe.counter \
              USING (tenant_id = current_setting('app.tenant', true)) \
              WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
            REVOKE ALL ON fresh_only_probe.counter FROM PUBLIC, wamn_app; \
            GRANT USAGE ON SCHEMA fresh_only_probe TO wamn_app; \
            GRANT SELECT (tenant_id, count), UPDATE (count) ON fresh_only_probe.counter TO wamn_app;").await?;
        project
            .execute(
                "INSERT INTO fresh_only_probe.counter VALUES ($1, 0), ($2, 0)",
                &[&super::identity().tenant.as_str(), &super::FOREIGN_TENANT],
            )
            .await?;
        let mut guest_config = project_config.clone();
        guest_config.user(&role).password(&password);
        let (guest, driver) = guest_config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(|_| anyhow::anyhow!("connect actual non-superuser tenant login"))?;
        let guest_task = tokio::spawn(async move {
            let _ = driver.await;
        });
        let actual_login: bool = guest
            .query_one(
                "SELECT current_user = $1 AND session_user = $1 \
            AND NOT rolsuper AND NOT rolbypassrls \
            AND current_setting('app.tenant', true) IS NULL \
            AND wamn_authority.current_tenant_key() = wamn_authority.tenant_key($2) \
            FROM pg_roles WHERE rolname = current_user",
                &[&role, &super::identity().tenant.as_str()],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            actual_login,
            "test must use the actual restricted tenant login without app.tenant"
        );
        let original = guest.execute(
            "UPDATE fresh_only_probe.counter SET count = count + 1 WHERE tenant_id = current_setting('app.tenant')",
            &[],
        ).await;
        let original_result = match original {
            Ok(0) => "zero-rows",
            Err(error)
                if error.code() == Some(&tokio_postgres::error::SqlState::UNDEFINED_OBJECT) =>
            {
                "42704"
            }
            _ => anyhow::bail!("the original GUC fixture did not reproduce its specific refusal"),
        };
        let unchanged: bool = project
            .query_one(
                "SELECT count(*) = 2 AND bool_and(count = 0) FROM fresh_only_probe.counter",
                &[],
            )
            .await?
            .get(0);
        anyhow::ensure!(unchanged, "original refused update changed committed data");
        println!("FRESH_ONLY_COUNTER_GUC original_result={original_result}");
        project
            .batch_execute("DROP POLICY prior_commit_tenant ON fresh_only_probe.counter")
            .await?;
        project.batch_execute(super::COUNTER_POLICY_SQL).await?;
        assert_eq!(
            guest.execute(COUNTER_SQL, &[]).await?,
            1,
            "login-bound floor must allow one own row without any tenant GUC"
        );
        for expected in [1_i64, 2] {
            if expected == 2 {
                guest.batch_execute("BEGIN").await?;
                guest
                    .query_one(
                        "SELECT set_config('app.tenant', $1, true)",
                        &[&super::FOREIGN_TENANT],
                    )
                    .await?;
                assert_eq!(
                    guest.execute(COUNTER_SQL, &[]).await?,
                    1,
                    "a forged tenant GUC must not change the login's one-row authority"
                );
                assert_eq!(
                    guest
                        .execute(
                            &format!("{COUNTER_SQL} WHERE tenant_id = $1"),
                            &[&super::FOREIGN_TENANT]
                        )
                        .await?,
                    0,
                    "forged GUC authorized another tenant"
                );
                guest.batch_execute("COMMIT").await?;
            }
            let rows = project
                .query("SELECT tenant_id, count FROM fresh_only_probe.counter", &[])
                .await?;
            anyhow::ensure!(rows.len() == 2, "counter row set changed");
            for row in rows {
                let tenant: String = row.get(0);
                let count: i64 = row.get(1);
                let expected_count = if tenant == super::identity().tenant.as_str() {
                    expected
                } else {
                    anyhow::ensure!(tenant == super::FOREIGN_TENANT, "unexpected counter tenant");
                    0
                };
                anyhow::ensure!(
                    count == expected_count,
                    "login-bound update changed the wrong committed row"
                );
            }
        }
        guest_task.abort();
        project_task.abort();
        admin_task.abort();
        println!(
            "FRESH_ONLY_COUNTER_LOGIN result=pass own_updates=2 foreign_updates=0 forged_guc=refused"
        );
        Ok(())
    }

    /// Isolate the compiled guest's forwarding from DB/authorization setup.
    #[tokio::test]
    #[ignore = "requires: WAMN_PRIOR_COMMIT_COMPONENT"]
    async fn counter_parent_executes_sql_then_forwards_the_exact_nested_call() -> anyhow::Result<()>
    {
        wamn_test_postgres::require_prerequisites(&["WAMN_PRIOR_COMMIT_COMPONENT"]);
        let engine = Engine::default();
        let component = Component::new(&engine, parent_component()?)?;
        let context = NodeContext {
            wiring_id: "abi-wiring".to_owned(),
            wiring_version: 7,
            node_id: "abi-parent".to_owned(),
            delivery_id: "abi-delivery".to_owned(),
            input_port: Some("input".to_owned()),
            occurrence: 3,
            traceparent: Some("00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned()),
            tracestate: Some("fixture=value".to_owned()),
            deadline_ms: Some(30_000),
            config: "{\"fixture\":true}".to_owned(),
        };
        let input = r#"[{"request_id":"abi-only","value":{"idempotency_key":"abi-key","purchase_order_id":"00000000-0000-0000-0000-000000000301","receipt_reference":"ABI-1","occurred_at":"2026-08-31T12:30:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000501","quantity":"5.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#;
        for refuse_nested in [false, true] {
            let mut linker = Linker::<Calls>::new(&engine);
            wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
            linker.instance("wamn:node/types@0.1.0")?;
            linker.instance("wamn:postgres/types@0.3.0")?;
            linker
                .instance("wamn:postgres/client@0.3.0")?
                .func_wrap_concurrent(
                    "execute",
                    |accessor, (sql, params): (String, Vec<SqlValue>)| {
                        Box::pin(async move {
                            assert_eq!(sql, COUNTER_SQL);
                            assert!(params.is_empty());
                            accessor.with(|mut store| store.data_mut().order.push("sql"));
                            Ok((Ok::<u64, PgError>(1),))
                        })
                    },
                )?;
            let nested_result = vec![RecordReceiptOutcome {
                request_id: "abi-only".to_owned(),
                outcome: Ok(RecordReceiptResult {
                    receipt_id: "00000000-0000-0000-0000-000000000601".to_owned(),
                    purchase_order_id: "00000000-0000-0000-0000-000000000301".to_owned(),
                    purchase_order_status: "received".to_owned(),
                    row_version: 2,
                }),
            }];
            linker
                .instance(*BASE_RECORD_RECEIPT)?
                .func_wrap_concurrent(
                    "run",
                    move |accessor, (context, input): (NodeContext, Vec<RecordReceiptItem>)| {
                        let nested_result = nested_result.clone();
                        Box::pin(async move {
                            accessor.with(|mut store| {
                                store.data_mut().order.push("nested");
                                store.data_mut().nested = Some((context, input.clone()));
                            });
                            if refuse_nested {
                                return Err(wash_runtime::wasmtime::Error::msg(
                                    "local nested refusal sentinel",
                                ));
                            }
                            Ok((Ok::<Vec<RecordReceiptOutcome>, NodeError>(nested_result),))
                        })
                    },
                )?;
            let mut store = Store::new(&engine, Calls::default());
            let instance = linker.instantiate_async(&mut store, &component).await?;
            let handler = instance
                .get_export_index(&mut store, None, OPERATION)
                .context("fixture handler export")?;
            let export = instance
                .get_export_index(&mut store, Some(&handler), "run")
                .context("fixture run export")?;
            let run: TypedFunc<(NodeContext, String), (Result<Emission, NodeError>,)> =
                instance.get_typed_func(&mut store, export)?;
            let result = match store
                .run_concurrent(async |accessor| {
                    run.call_concurrent(accessor, (context.clone(), input.to_owned()))
                        .await
                })
                .await
            {
                Ok(result) => result,
                Err(error) => Err(error),
            };
            if refuse_nested {
                let error = result.expect_err("the nested host error must propagate");
                anyhow::ensure!(
                    format!("{error:#}").contains("local nested refusal sentinel"),
                    "the fixture trapped before reaching the nested refusal: {error:#}"
                );
            } else {
                let (result,) = result
                    .map_err(anyhow::Error::from)
                    .context("execute the compiled counter parent")?;
                assert_eq!(
                    result,
                    Ok(Emission {
                        payload: r#"[{"request_id":"abi-only","value":{"purchase_order_id":"00000000-0000-0000-0000-000000000301","purchase_order_status":"received","receipt_id":"00000000-0000-0000-0000-000000000601","row_version":2}}]"#.to_owned(),
                        port: None,
                    })
                );
            }
            assert_eq!(store.data().order, ["sql", "nested"]);
            assert_eq!(
                store.data().nested,
                Some((
                    context.clone(),
                    vec![RecordReceiptItem {
                        request_id: "abi-only".to_owned(),
                        input: Ok(RecordReceiptRequest {
                            idempotency_key: "abi-key".to_owned(),
                            line: vec![bindings::wamn_receiving::receiving::record_receipt::RecordReceiptLine {
                                location_id: "00000000-0000-0000-0000-000000000201".to_owned(),
                                purchase_order_line_id: "00000000-0000-0000-0000-000000000501".to_owned(),
                                quantity: "5.0000".to_owned(),
                            }],
                            occurred_at: "2026-08-31T12:30:00.000000Z".to_owned(),
                            purchase_order_id: "00000000-0000-0000-0000-000000000301".to_owned(),
                            receipt_reference: "ABI-1".to_owned(),
                        }),
                    }],
                ))
            );
        }
        Ok(())
    }
}
