//! A real nested refusal after one independently committed guest SQL effect.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use serde_json::{Value, json};
use tokio_postgres::Client;
use wamn_catalog::{ComponentDeclaration, PackageCoordinate};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{self, AuthorWiringArgs};
use wamn_ctl::dev::environment::{
    ENVIRONMENT, JourneyCredentials, ORG, PROJECT, TENANT, connect, secret_value,
    spawn_journey_management_gate,
};
use wamn_ctl::publish_release::{self, PublishReleaseArgs, ReleaseWiringTarget};
use wamn_ctl::push_component::{self, PushComponentArgs};
use wamn_ctl::push_release_manifest::{self, PushReleaseManifestArgs};
use wamn_platform_identity::{issue_pat, revoke_pat};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;
use wamn_runtime::session_verifier::SessionVerifier;

use super::{
    BASE_PACKAGE_ID, BASE_PACKAGE_VERSION, BASE_RECORD_RECEIPT, JOURNEY_PACKAGES, JourneyDocument,
    ROUTE_JOURNEY_GATE_BIND, ScratchRoot, TraceHarness, assert_invocation_identity,
    assert_operation_refusal, assert_postgres_descendants, build_journey_runtime,
    invoke_journey_route, journey_package_root, journey_publication_root,
    journey_scenario_worker_binary, journey_trace, span_attribute, span_descends_from,
    successful_value, trace_component_invocations,
};

const PACKAGE: &str = "fresh_only_probe";
const VERSION: &str = "1.0.0";
const OPERATION: &str = "wamn:node/handler@0.1.0";
const WIRING: &str = "prior_commit";
const ROUTE: &str = "/fresh_only_probe/prior_commit";
const COUNTER_ID: &str = "00000000-0000-0000-0000-000000000904";
const COUNTER_SQL: &str = "UPDATE fresh_only_probe.counter SET count = count + 1 WHERE tenant_id = current_setting('app.tenant')";

pub(super) struct Proof<'a> {
    pub inputs: &'a JourneyDocument,
    pub credentials: &'a JourneyCredentials,
    pub project_url: &'a str,
    pub project: &'a Client,
    pub control: &'a Client,
    pub publisher: &'a str,
    pub verifier: SessionVerifier,
    pub session: &'a str,
    pub pat: &'a str,
    pub body: Bytes,
    pub expected_base: &'a Value,
    pub human_id: &'a str,
    pub traces: &'a TraceHarness,
}

impl std::fmt::Debug for Proof<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Proof").finish_non_exhaustive()
    }
}

pub(super) async fn prove_prior_commit(proof: Proof<'_>) -> anyhow::Result<()> {
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let package = root.join(PACKAGE);
    std::fs::create_dir(&package)?;
    std::fs::create_dir(package.join("migrations"))?;
    let snapshots = release_snapshots(proof.project).await?;
    anyhow::ensure!(
        snapshots.len() == 3,
        "prior-commit fixture requires releases 1 through 3"
    );
    let previous = ReleaseManifestWeld::load_canonical_bytes(&snapshots[2].1, "release 3")?;
    let base = previous
        .manifest()
        .components
        .iter()
        .find(|component| component.package_id == BASE_PACKAGE_ID)
        .context("prior-commit release lacks the actual Receiving component")?;
    anyhow::ensure!(
        base.operations
            .get(BASE_RECORD_RECEIPT)
            .is_some_and(|operation| operation.fresh_only),
        "prior-commit fixture requires the released fresh-only base"
    );
    let base_digest = base.digest.as_str();
    let manifest = fixture_manifest(base_digest);
    write_json(&package.join("wamn.json"), &manifest)?;
    std::fs::write(
        package.join("migrations/0001_counter.sql"),
        "CREATE TABLE fresh_only_probe.counter (\n\
         id uuid CONSTRAINT counter_id_pkey PRIMARY KEY,\n\
         tenant_id text NOT NULL, count bigint NOT NULL);\n",
    )?;
    apply_package::run(ApplyPackageArgs {
        package: package.clone(),
        database_url: proof.project_url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await?;
    wamn_schema_generator::materialize_package_verified(
        wamn_schema_generator::MaterializeMode::Write,
        proof.project_url,
        &package,
    )
    .await?;
    let counter_read = std::fs::read_to_string(package.join("generated/sql/counter/get.sql"))?;
    // Test instrumentation, not a generated application command: the ordinary
    // raw palette import uses GuestSql and its tenant claim. Only count is
    // writable; this fixture adds no role or permanent product policy.
    proof
        .project
        .batch_execute(
            "ALTER TABLE fresh_only_probe.counter ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE fresh_only_probe.counter FORCE ROW LEVEL SECURITY; \
         CREATE POLICY prior_commit_tenant ON fresh_only_probe.counter \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         REVOKE ALL ON fresh_only_probe.counter FROM PUBLIC, wamn_app; \
         GRANT USAGE ON SCHEMA fresh_only_probe TO wamn_app; \
         GRANT SELECT (id, tenant_id, count), UPDATE (count) \
           ON fresh_only_probe.counter TO wamn_app;",
        )
        .await?;
    proof
        .project
        .execute(
            "INSERT INTO fresh_only_probe.counter (id, tenant_id, count) \
         VALUES ('00000000-0000-0000-0000-000000000904', $1, 0)",
            &[&TENANT],
        )
        .await?;
    assert_counter_authority(&proof).await?;

    let source: Value = serde_json::from_slice(&std::fs::read(
        journey_publication_root(JOURNEY_PACKAGES[0]).join("components/receiving.json.in"),
    )?)?;
    let ports = &source["operations"][BASE_RECORD_RECEIPT];
    let declaration = json!({
        "scope": {"tenant-id": TENANT, "package-id": PACKAGE, "package-version": VERSION},
        "component": WIRING, "interface-version": "0.1.0", "connections": [],
        "operations": {(OPERATION): {
            "fresh-only": false,
            "dependencies": [{"package": BASE_PACKAGE_ID, "version": BASE_PACKAGE_VERSION,
                "digest": base_digest, "operation": BASE_RECORD_RECEIPT}],
            "input-ports": ports["input-ports"], "output-ports": ports["output-ports"],
            "parameters": []
        }}
    });
    let declaration_path = root.join("parent.json");
    write_json(&declaration_path, &declaration)?;
    let bytes = parent_component()?;
    let parent_digest = wamn_runtime::component_admission::component_digest(&bytes);
    let component_path = root.join("parent.wasm");
    std::fs::write(&component_path, bytes)?;
    push_component::run(PushComponentArgs {
        package: package.clone(),
        component_bytes: component_path,
        declaration: declaration_path,
        artifact_base: proof.inputs.component_artifact_base.clone(),
        registry_auth_file: proof.inputs.registry_auth_file.clone(),
        insecure_registry: true,
        admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
        project_database_url: proof.project_url.to_owned(),
        control_database_url: proof.inputs.system_pg_url.clone(),
    })
    .await?;
    let wiring = json!({"format-version": "0.1", "wiring-id": WIRING, "version": 1,
        "entry": "parent", "nodes": {"parent": {"component": WIRING,
            "interface-version": "0.1.0", "operation": OPERATION, "terminal": "respond"}}});
    let wiring_path = root.join("wiring.json");
    write_json(&wiring_path, &wiring)?;
    gate_wiring(&proof, &wiring).await?;
    author_wiring::run(AuthorWiringArgs {
        database_url: proof.project_url.to_owned(),
        control_database_url: proof.inputs.system_pg_url.clone(),
        tenant: TENANT.to_owned(),
        package_id: PACKAGE.to_owned(),
        package_version: VERSION.to_owned(),
        wiring_document: wiring_path,
    })
    .await?;

    let definition = json!({"id": WIRING, "kind": "http",
        "route": {"path": ROUTE, "method": "POST"},
        "raw-body-bytes": {"maximum": 1048576},
        "input-schema": ports["input-ports"][0]["schema"]});
    let attachment = root.join("attachment.json");
    write_json(
        &attachment,
        &json!({(WIRING): {
            "kind": "http", "package-id": PACKAGE, "wiring-id": WIRING, "wiring-version": 1,
            "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
            "definition": definition, "auth-policy": {"modes": ["pat", "session"]}
        }}),
    )?;
    let mut packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| PackageCoordinate::new(package.id, package.version))
        .collect::<Result<Vec<_>, _>>()?;
    packages.push(PackageCoordinate::new(PACKAGE, VERSION)?);
    let mut manifests = JOURNEY_PACKAGES
        .iter()
        .map(|package| journey_package_root(*package).join("wamn.json"))
        .collect::<Vec<_>>();
    manifests.push(package.join("wamn.json"));
    publish_release::run(PublishReleaseArgs {
        database_url: proof.project_url.to_owned(),
        control_database_url: proof.inputs.system_pg_url.clone(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: 4,
        environment: ENVIRONMENT.to_owned(),
        verified_publisher_principal: proof.publisher.to_owned(),
        run_schema: "wamn_run".to_owned(),
        packages,
        wirings: vec![
            format!("{PACKAGE}@{VERSION}::{WIRING}=1")
                .parse::<ReleaseWiringTarget>()
                .map_err(anyhow::Error::msg)?,
        ],
        attachments: vec![attachment],
        route_host: Some(proof.inputs.route_host.clone()),
        package_manifests: manifests,
    })
    .await?;
    push_release_manifest::run(PushReleaseManifestArgs {
        database_url: proof.project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: 4,
        artifact_base: proof.inputs.release_artifact_base.clone(),
        registry_auth_file: proof.inputs.registry_auth_file.clone(),
        insecure_registry: true,
        control_database_url: proof.inputs.system_pg_url.clone(),
    })
    .await?;
    let digest: String = proof
        .project
        .query_one(
            "SELECT manifest_digest FROM catalog.release_manifest_v3_snapshots \
         WHERE tenant_id = $1 AND effective_release_id = 4",
            &[&TENANT],
        )
        .await?
        .get(0);
    let source = ReleaseManifestSource::new(
        &proof.inputs.release_artifact_base,
        true,
        &proof.inputs.registry_auth_file,
    )?;
    let released_bytes = source.pull_verified(&digest).await?;
    let release = Arc::new(ReleaseManifestWeld::load_canonical_bytes(
        &released_bytes,
        &format!("{}@{digest}", proof.inputs.release_artifact_base),
    )?);
    anyhow::ensure!(
        release.manifest().format_version == 1
            && release.release().effective_release_id == 4
            && release
                .manifest()
                .components
                .iter()
                .any(|component| component.digest.as_str() == parent_digest
                    && component
                        .operations
                        .get(OPERATION)
                        .is_some_and(|operation| !operation.fresh_only
                            && operation.registered_operation.is_none()))
            && release
                .manifest()
                .components
                .iter()
                .any(|component| component == base),
        "prior-commit release changed its parent or actual fresh-only dependency"
    );
    anyhow::ensure!(
        release_snapshots(proof.project).await? == snapshots,
        "prior-commit publication changed an earlier immutable release"
    );

    let mut runtime_inputs = JourneyDocument::required()?;
    runtime_inputs.compilation_cache_directory = root.join("compilation-cache");
    std::fs::create_dir(&runtime_inputs.compilation_cache_directory)?;
    let (engine, flow_http, routing, bridge, identity_task) = build_journey_runtime(
        &runtime_inputs,
        proof.credentials,
        release,
        Some(proof.verifier.clone()),
    )
    .await?;
    let result = async {
        anyhow::ensure!(
            counter(&proof, &counter_read).await? == 0,
            "counter must start at zero"
        );
        let (trace, parent) = journey_trace(41);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &proof.inputs.route_host,
            ROUTE,
            Some(proof.session),
            &parent,
            proof.body.clone(),
        )
        .await?;
        assert_operation_refusal(&response, "fresh-credential-required", BASE_RECORD_RECEIPT)?;
        anyhow::ensure!(
            counter(&proof, &counter_read).await? == 1,
            "late session refusal rolled back or repeated the earlier committed effect"
        );
        assert_counter_trace(
            &proof,
            &trace,
            &parent_digest,
            base_digest,
            "session",
            false,
        )?;
        let (trace, parent) = journey_trace(42);
        let response = invoke_journey_route(
            &engine,
            &flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            &proof.inputs.route_host,
            ROUTE,
            Some(proof.pat),
            &parent,
            proof.body.clone(),
        )
        .await?;
        anyhow::ensure!(
            successful_value(&response, "session-nested-replay")? == *proof.expected_base,
            "explicit fresh PAT did not return the independent stored base result"
        );
        anyhow::ensure!(
            counter(&proof, &counter_read).await? == 2,
            "explicit PAT must add exactly one further committed effect"
        );
        assert_counter_trace(&proof, &trace, &parent_digest, base_digest, "pat", true)?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    identity_task.abort();
    result?;
    println!(
        "FRESH_ONLY_PRIOR_COMMIT result=pass session_counter=1 pat_counter=2 fixture_release=4"
    );
    Ok(())
}

fn assert_counter_trace(
    proof: &Proof<'_>,
    trace: &str,
    parent_digest: &str,
    base_digest: &str,
    credential: &str,
    reached_base: bool,
) -> anyhow::Result<()> {
    let spans = proof.traces.spans();
    let invoked = trace_component_invocations(&spans, trace);
    anyhow::ensure!(
        invoked.len() == if reached_base { 2 } else { 1 },
        "counter wiring repeated a component or dispatched a refused child"
    );
    let parent = invoked
        .iter()
        .copied()
        .find(|span| span_attribute(span, "wamn.operation").as_deref() == Some(OPERATION))
        .context("counter parent invocation missing")?;
    assert_invocation_identity(
        parent,
        trace,
        WIRING,
        OPERATION,
        parent_digest,
        proof.human_id,
    );
    assert_postgres_descendants(&spans, trace, parent);
    for invocation in &invoked {
        anyhow::ensure!(
            span_attribute(invocation, "wamn.caller_credential_kind").as_deref()
                == Some(credential),
            "counter wiring changed the original credential kind"
        );
    }
    if reached_base {
        let base = invoked
            .iter()
            .copied()
            .find(|span| {
                span_attribute(span, "wamn.operation").as_deref() == Some(BASE_RECORD_RECEIPT)
            })
            .context("explicit PAT did not enter the actual base")?;
        assert_invocation_identity(
            base,
            trace,
            WIRING,
            BASE_RECORD_RECEIPT,
            base_digest,
            proof.human_id,
        );
        anyhow::ensure!(
            span_descends_from(&spans, base, parent),
            "base invocation lost parent ancestry"
        );
        assert_postgres_descendants(&spans, trace, base);
    }
    Ok(())
}

async fn counter(proof: &Proof<'_>, generated_get: &str) -> anyhow::Result<i64> {
    // Exercise the package's actual generated read through the scoped reader,
    // then compare it with an independent owner query over the committed row.
    let (guest, task) = connect(&proof.credentials.guest_sql).await?;
    let result = async {
        guest.batch_execute("BEGIN; SET LOCAL search_path = fresh_only_probe, public").await?;
        guest.query_one("SELECT set_config('app.tenant', $1, true)", &[&TENANT]).await?;
        let row = guest.query_typed_one(generated_get,
            &[(&COUNTER_ID, tokio_postgres::types::Type::TEXT)]).await?;
        let count: i64 = row.try_get("count")?;
        anyhow::ensure!(row.try_get::<_, String>("tenant_id")? == TENANT,
            "generated counter read crossed tenant scope");
        guest.batch_execute("ROLLBACK").await?;
        let independent: i64 = proof.project.query_one(
            "SELECT count FROM fresh_only_probe.counter WHERE tenant_id = $1 AND id = $2::text::uuid",
            &[&TENANT, &COUNTER_ID]).await?.get(0);
        anyhow::ensure!(count == independent, "generated and independent counter reads disagree");
        Ok::<_, anyhow::Error>(count)
    }.await;
    task.abort();
    result
}

async fn release_snapshots(project: &Client) -> anyhow::Result<Vec<(i32, Vec<u8>)>> {
    Ok(project
        .query(
            "SELECT effective_release_id, canonical_bytes \
        FROM catalog.release_manifest_v3_snapshots WHERE tenant_id = $1 \
        AND effective_release_id BETWEEN 1 AND 3 ORDER BY effective_release_id",
            &[&TENANT],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

async fn assert_counter_authority(proof: &Proof<'_>) -> anyhow::Result<()> {
    let (guest, task) = connect(&proof.credentials.guest_sql).await?;
    let result = async {
        let allowed: bool = guest.query_one("SELECT \
            has_column_privilege(current_user, 'fresh_only_probe.counter', 'count', 'UPDATE') \
            AND NOT has_column_privilege(current_user, 'fresh_only_probe.counter', 'id', 'UPDATE') \
            AND NOT has_column_privilege(current_user, 'fresh_only_probe.counter', 'tenant_id', 'UPDATE') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'INSERT') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'DELETE') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'TRUNCATE')", &[]).await?.get(0);
        anyhow::ensure!(allowed, "fixture counter grants exceed its one effect column");
        guest.batch_execute("BEGIN; SELECT set_config('app.tenant', 'wrong-prior-commit-tenant', true)").await?;
        let changed = guest.execute("UPDATE fresh_only_probe.counter SET count = count + 1", &[]).await?;
        guest.batch_execute("ROLLBACK").await?;
        anyhow::ensure!(changed == 0, "counter update escaped the real GuestSql tenant RLS");
        Ok::<_, anyhow::Error>(())
    }.await;
    task.abort();
    result
}

async fn gate_wiring(proof: &Proof<'_>, wiring: &Value) -> anyhow::Result<()> {
    let copies = proof
        .inputs
        .fresh_only_packages
        .as_ref()
        .context("fresh-only authority copies missing")?;
    let credentials = JourneyCredentials {
        guest_sql: proof.credentials.guest_sql.clone(),
        executor_platform: proof.credentials.executor_platform.clone(),
        event_materializer: proof.credentials.event_materializer.clone(),
        http_admitter: proof.credentials.http_admitter.clone(),
        identity_reader: proof.credentials.identity_reader.clone(),
        control_author: secret_value(&copies.join("control-author.json"), "url")?,
        management_admitter: secret_value(&copies.join("management-admitter.json"), "url")?,
    };
    let pat = issue_pat(
        proof.control,
        &proof.publisher.parse()?,
        "prior-commit Gate fixture",
        Duration::from_secs(300),
    )
    .await?;
    let mut gate = spawn_journey_management_gate(
        &journey_scenario_worker_binary()?,
        &credentials,
        &credentials.management_admitter,
        ROUTE_JOURNEY_GATE_BIND,
    )
    .await?;
    let result = async {
        let response = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?
            .post(format!("http://{}/authoring", gate.bind()))
            .bearer_auth(pat.token())
            .json(
                &json!({"document": "request", "body": {"schema-version": "0.1",
                "command-id": "gate-prior-commit-fixture", "command": {"kind": "gate", "input": {
                    "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                    "package-id": PACKAGE, "package-version": VERSION, "document": wiring
                }}}}),
            )
            .send()
            .await?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("fixture Gate returned invalid JSON"))?;
        anyhow::ensure!(
            status.is_success() && body["body"]["outcome"]["status"] == "completed",
            "real management Gate refused the prior-commit wiring"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let stopped = gate.shutdown().await;
    let revoked = revoke_pat(proof.control, pat.record().prefix()).await;
    result?;
    stopped?;
    revoked?;
    Ok(())
}

fn write_json(path: &std::path::Path, value: &Value) -> anyhow::Result<()> {
    std::fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}

fn fixture_manifest(base_digest: &str) -> Value {
    json!({
        "package": {"id": PACKAGE, "version": VERSION},
        "base_dependencies": {"base_receiving": {
            "package": BASE_PACKAGE_ID, "version": BASE_PACKAGE_VERSION,
            "digest": base_digest, "operations": ["receiving.record_receipt"]
        }},
        "required_platform_policy_contract": {"id": "prior_commit_fixture", "state": "satisfied"},
        "models": {"counter": {
            "schema": PACKAGE, "table": "counter", "owner": PACKAGE,
            "operations": {"get": {
                "permission": "counter.get", "result": "one",
                "error_details": {
                    "invalid_input": {"required": ["field"]},
                    "not_found": {"required": ["field", "id"]},
                    "retry": {}, "timeout": {},
                    "permission_denied": {"required": ["operation"]}, "internal_error": {}
                }
            }}
        }},
        "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
        "components": {"data": {"connections": ["postgres"]}}
    })
}

fn parent_component() -> anyhow::Result<Vec<u8>> {
    // More than 16 flat handler parameters use one pointer to the original
    // (node-context, input) aggregate. Forward it unchanged after SQL COMMIT.
    Ok(wat::parse_str(format!(
        r#"(component
      (import "wamn:node/types@0.1.0" (instance $node
        (type $json' string)
        (export "json" (type $json (eq $json')))
        (type $context' (record
          (field "wiring-id" string) (field "wiring-version" u32)
          (field "node-id" string) (field "delivery-id" string)
          (field "input-port" (option string)) (field "occurrence" u32)
          (field "traceparent" (option string)) (field "tracestate" (option string))
          (field "deadline-ms" (option u64)) (field "config" $json)))
        (export "node-context" (type $context (eq $context')))
        (type $detail' (record (field "message" string) (field "code" (option string))))
        (export "error-detail" (type $detail (eq $detail')))
        (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (export "rate-limit-detail" (type $rate (eq $rate')))
        (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
          (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (export "node-error" (type $error (eq $error')))
        (type $emission' (record (field "payload" $json) (field "port" (option string))))
        (export "emission" (type $emission (eq $emission')))))
      (alias export $node "json" (type $json))
      (alias export $node "node-context" (type $context))
      (alias export $node "node-error" (type $error))
      (alias export $node "emission" (type $emission))
      (import "{base}" (instance $base
        (export "json" (type (eq $json)))
        (export "node-context" (type (eq $context)))
        (export "emission" (type (eq $emission)))
        (export "node-error" (type (eq $error)))
        (export "run" (func (param "ctx" $context) (param "input" $json)
          (result (result $emission (error $error)))))))
      (import "wamn:postgres/types@0.1.0" (instance $pg
        (type $value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
          (case "int64" s64) (case "float64" f64) (case "text" string)
          (case "bytes" (list u8)) (case "numeric" string) (case "timestamptz" string)
          (case "json" string) (case "uuid" string)))
        (export "sql-value" (type $value (eq $value')))
        (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
          (case "statement-timeout") (case "row-limit-exceeded" u64)
          (case "unique-violation" string) (case "foreign-key-violation" string)
          (case "check-violation" string) (case "exclusion-violation" string)
          (case "permission-denied") (case "query-error" (tuple string string))))
        (export "pg-error" (type $pg-error (eq $pg-error')))))
      (alias export $pg "sql-value" (type $value))
      (alias export $pg "pg-error" (type $pg-error))
      (import "wamn:postgres/client@0.1.0" (instance $pg-client
        (export "sql-value" (type (eq $value))) (export "pg-error" (type (eq $pg-error)))
        (export "execute" (func (param "sql" string) (param "params" (list $value))
          (result (result u64 (error $pg-error)))))))
      (core module $memory
        (memory (export "memory") 16)
        (global $next (mut i32) (i32.const 1024))
        (data (i32.const 256) "{sql}")
        (func (export "realloc") (param $old i32) (param $old-size i32)
          (param $align i32) (param $size i32) (result i32) (local $new i32)
          global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $new
          local.get $size i32.add global.set $next
          global.get $next i32.const 1048576 i32.gt_u if unreachable end
          local.get $old if
            local.get $new local.get $old local.get $old-size memory.copy
          end local.get $new))
      (core instance $memory (instantiate $memory))
      (core func $execute (canon lower (func $pg-client "execute")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $nested (canon lower (func $base "run")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "execute" (func $execute (param i32 i32 i32 i32 i32)))
        (import "host" "nested" (func $nested (param i32 i32)))
        (func (export "run") (param $input i32) (result i32)
          i32.const 256 i32.const {sql_len} i32.const 192 i32.const 0 i32.const 768 call $execute
          i32.const 768 i32.load8_u if unreachable end
          i32.const 776 i64.load i64.const 1 i64.ne if unreachable end
          local.get $input i32.const 832 call $nested
          i32.const 832))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "execute" (func $execute)) (export "nested" (func $nested))))))
      (func $run (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "emission" (type $emission)) (export "node-error" (type $error))
        (export "run" (func $run)))
      (export "{operation}" (instance $handler)))"#,
        base = BASE_RECORD_RECEIPT,
        operation = OPERATION,
        sql = COUNTER_SQL,
        sql_len = COUNTER_SQL.len()
    ))?)
}

#[test]
fn counter_parent_has_the_real_node_and_nested_operation_abi() -> anyhow::Result<()> {
    let manifest = wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(
        &fixture_manifest(&format!("sha256:{}", "a".repeat(64))),
    )?)?;
    let operations = wamn_schema_generator::validate_operation_vocabulary(&manifest)?;
    anyhow::ensure!(
        operations == ["counter.get".to_owned()].into(),
        "fixture must declare exactly the generated counter read it exercises"
    );
    let engine = wamn_runtime::engine::build_engine(&[])?;
    let declaration: ComponentDeclaration = serde_json::from_value(json!({
        "scope": {"tenant-id": "fixture", "package-id": PACKAGE, "package-version": VERSION},
        "component": WIRING, "interface-version": "0.1.0", "connections": [],
        "operations": {(OPERATION): {"dependencies": [{"package": BASE_PACKAGE_ID,
            "version": BASE_PACKAGE_VERSION, "digest": format!("sha256:{}", "a".repeat(64)),
            "operation": BASE_RECORD_RECEIPT}],
            "input-ports": [{"name": "input", "schema": {"type": "array"}}],
            "output-ports": [{"name": "main", "schema": {"type": "array"}}], "parameters": []}}
    }))?;
    let admitted = wamn_runtime::component_admission::validate_component_admission(
        &engine,
        &parent_component()?,
        wamn_runtime::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: ["wamn:node".to_owned(), "wamn:postgres".to_owned()].into(),
            effect_free_operation_dependencies: Default::default(),
        },
    )?;
    anyhow::ensure!(
        !admitted.component.operations[OPERATION].fresh_only,
        "instrumentation parent must remain ordinary"
    );
    Ok(())
}
