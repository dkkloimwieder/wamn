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
use wamn_platform_identity::{PrincipalKind, issue_pat, resolve_subject, revoke_pat};
use wamn_runtime::release_manifest::LoadedRelease;
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
const FOREIGN_COUNTER_ID: &str = "00000000-0000-0000-0000-000000000905";
const FOREIGN_TENANT: &str = "prior-commit-other-tenant";
const COUNTER_SQL: &str = "UPDATE fresh_only_probe.counter SET count = count + 1";
const COUNTER_POLICY_SQL: &str = "CREATE POLICY prior_commit_tenant ON fresh_only_probe.counter \
    TO wamn_app \
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) \
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())";

pub(super) struct PriorCommitTest<'a> {
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
    pub client_credentials: Option<Arc<dyn wamn_client::CredentialProvider>>,
}

impl std::fmt::Debug for PriorCommitTest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PriorCommitTest").finish_non_exhaustive()
    }
}

pub(super) async fn test_prior_commit(test: PriorCommitTest<'_>) -> anyhow::Result<()> {
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let package = root.join(PACKAGE);
    std::fs::create_dir(&package)?;
    std::fs::create_dir(package.join("migrations"))?;
    let snapshots = release_snapshots(test.project).await?;
    anyhow::ensure!(
        snapshots.len() == 3,
        "prior-commit fixture requires releases 1 through 3"
    );
    let previous = LoadedRelease::load_canonical_bytes(&snapshots[2].1, "release 3")?;
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
        database_url: test.project_url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await?;
    wamn_schema_generator::materialize_package_verified(
        wamn_schema_generator::MaterializeMode::Write,
        test.project_url,
        &package,
    )
    .await?;
    let counter_read = std::fs::read_to_string(package.join("generated/sql/counter/get.sql"))?;
    // Test instrumentation, not a generated application command: the ordinary
    // raw palette import uses GuestSql and its login-bound tenant. Only count is
    // writable; this fixture adds no role or permanent product policy.
    test
        .project
        .batch_execute(&format!(
            "ALTER TABLE fresh_only_probe.counter ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE fresh_only_probe.counter FORCE ROW LEVEL SECURITY; \
         {COUNTER_POLICY_SQL}; \
         REVOKE ALL ON fresh_only_probe.counter FROM PUBLIC, wamn_app; \
         GRANT USAGE ON SCHEMA fresh_only_probe TO wamn_app; \
         GRANT SELECT (id, tenant_id, count), UPDATE (count) \
           ON fresh_only_probe.counter TO wamn_app;"
        ))
        .await?;
    test
        .project
        .execute(
            "INSERT INTO fresh_only_probe.counter (id, tenant_id, count) \
         VALUES ('00000000-0000-0000-0000-000000000904', $1, 0), \
                ('00000000-0000-0000-0000-000000000905', $2, 0)",
            &[&TENANT, &FOREIGN_TENANT],
        )
        .await?;
    assert_counter_authority(&test).await?;

    let base_package = *JOURNEY_PACKAGES
        .iter()
        .find(|package| package.id == BASE_PACKAGE_ID)
        .context("the journey must declare the actual base package")?;
    let source: Value = serde_json::from_slice(&std::fs::read(
        journey_publication_root(base_package, Some(test.inputs)).join("components/receiving.json.in"),
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
        artifact_base: test.inputs.component_artifact_base.clone(),
        registry_auth_file: test.inputs.registry_auth_file.clone(),
        insecure_registry: true,
        admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
        project_database_url: test.project_url.to_owned(),
        control_database_url: test.inputs.system_pg_url.clone(),
    })
    .await?;
    let wiring = json!({"format-version": "0.1", "wiring-id": WIRING, "version": 1,
        "entry": "parent", "nodes": {"parent": {"component": WIRING,
            "interface-version": "0.1.0", "operation": OPERATION, "terminal": "respond"}}});
    let wiring_path = root.join("wiring.json");
    write_json(&wiring_path, &wiring)?;
    gate_wiring(&test, &wiring).await?;
    author_wiring::run(AuthorWiringArgs {
        database_url: test.project_url.to_owned(),
        control_database_url: test.inputs.system_pg_url.clone(),
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
    // Acme's event handler is outside this wiring's actual dependency closure.
    // Including that package would also require its unrelated handler wiring.
    let packages = vec![
        PackageCoordinate::new(BASE_PACKAGE_ID, BASE_PACKAGE_VERSION)?,
        PackageCoordinate::new(PACKAGE, VERSION)?,
    ];
    let manifests = vec![
        journey_package_root(base_package, Some(test.inputs)).join("wamn.json"),
        package.join("wamn.json"),
    ];
    publish_release::run(PublishReleaseArgs {
        database_url: test.project_url.to_owned(),
        control_database_url: test.inputs.system_pg_url.clone(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: 4,
        environment: ENVIRONMENT.to_owned(),
        verified_publisher_principal: test.publisher.to_owned(),
        run_schema: "wamn_run".to_owned(),
        packages,
        wirings: vec![
            format!("{PACKAGE}@{VERSION}::{WIRING}=1")
                .parse::<ReleaseWiringTarget>()
                .map_err(anyhow::Error::msg)?,
        ],
        attachments: vec![attachment],
        route_host: Some(test.inputs.route_host.clone()),
        package_manifests: manifests,
    })
    .await?;
    push_release_manifest::run(PushReleaseManifestArgs {
        database_url: test.project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: 4,
        artifact_base: test.inputs.release_artifact_base.clone(),
        registry_auth_file: test.inputs.registry_auth_file.clone(),
        insecure_registry: true,
        control_database_url: test.inputs.system_pg_url.clone(),
    })
    .await?;
    let digest: String = test
        .project
        .query_one(
            "SELECT manifest_digest FROM catalog.release_manifest_v3_snapshots \
         WHERE tenant_id = $1 AND effective_release_id = 4",
            &[&TENANT],
        )
        .await?
        .get(0);
    let attested: String = test
        .control
        .query_one(
            "SELECT deployed_manifest_hash FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = 4 \
               AND org_id = $2 AND project_id = $3 AND environment = $4",
            &[&TENANT, &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await?
        .get(0);
    anyhow::ensure!(
        attested == digest,
        "prior-commit deployment attestation differs from its minted release"
    );
    let source = ReleaseManifestSource::new(
        &test.inputs.release_artifact_base,
        true,
        &test.inputs.registry_auth_file,
    )?;
    let released_bytes = source.pull_verified(&digest).await?;
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &released_bytes,
        &format!("{}@{digest}", test.inputs.release_artifact_base),
    )?);
    anyhow::ensure!(
        release.manifest().format_version == 1
            && release.release().effective_release_id == 4
            && release.manifest().release.packages.len() == 2
            && release.manifest().components.len() == 2
            && release.manifest().wirings.len() == 1
            && release.manifest().attachments.len() == 1
            && release.manifest().registrations.is_empty()
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
        release_snapshots(test.project).await? == snapshots,
        "prior-commit publication changed an earlier immutable release"
    );

    let mut runtime_inputs = JourneyDocument::required()?;
    runtime_inputs.compilation_cache_directory = root.join("compilation-cache");
    std::fs::create_dir(&runtime_inputs.compilation_cache_directory)?;
    let (engine, flow_http, routing, bridge, identity_task) = build_journey_runtime(
        &runtime_inputs,
        test.credentials,
        release,
        Some(test.verifier.clone()),
    )
    .await?;
    let client_transport = test.client_credentials.as_ref().map(|_| {
        super::session_client::RouteTransport::new(
            engine.clone(),
            flow_http.clone(),
            routing.clone(),
            bridge.clone(),
            41,
        )
    });
    let client = test
        .client_credentials
        .as_ref()
        .zip(client_transport.as_ref())
        .map(|(credentials, transport)| {
            super::session_client::client(
                credentials.clone(),
                transport.clone(),
                &test.inputs.route_host,
            )
        });
    let result = async {
        anyhow::ensure!(
            counter(&test, &counter_read).await? == 0,
            "counter must start at zero"
        );
        let (trace, parent) = journey_trace(41);
        let client_items: Vec<Value> = serde_json::from_slice(&test.body)?;
        if let Some(client) = &client {
            let result = client
                .invoke(
                    &super::session_client::route(ROUTE),
                    &std::collections::BTreeMap::new(),
                    &client_items,
                )
                .await;
            super::session_client::assert_fresh_refusal(result)?;
            anyhow::ensure!(
                client_transport
                    .as_ref()
                    .is_some_and(|transport| transport.calls() == 1),
                "late refusal caused an automatic client replay"
            );
        } else {
            let response = invoke_journey_route(
                &engine,
                &flow_http,
                Arc::clone(&routing),
                Arc::clone(&bridge),
                &test.inputs.route_host,
                ROUTE,
                Some(test.session),
                &parent,
                test.body.clone(),
            )
            .await?;
            let refusal = assert_operation_refusal(
                &response,
                "fresh-credential-required",
                BASE_RECORD_RECEIPT,
            );
            if refusal.is_err() {
                diagnose_prior_commit_failure(&test, &trace).await;
            }
            refusal?;
        }
        anyhow::ensure!(
            counter(&test, &counter_read).await? == 1,
            "late session refusal rolled back or repeated the earlier committed effect"
        );
        assert_counter_trace(
            &test,
            &trace,
            &parent_digest,
            base_digest,
            "session",
            false,
        )?;
        let (trace, parent) = journey_trace(42);
        if let Some(client) = &client {
            let result = client
                .invoke_fresh(
                    &super::session_client::route(ROUTE),
                    &std::collections::BTreeMap::new(),
                    &client_items,
                )
                .await;
            super::session_client::assert_value(
                result,
                "session-nested-replay",
                test.expected_base,
            )?;
            anyhow::ensure!(
                client_transport
                    .as_ref()
                    .is_some_and(|transport| transport.calls() == 2),
                "explicit fresh retry must send exactly one further client request"
            );
        } else {
            let response = invoke_journey_route(
                &engine,
                &flow_http,
                Arc::clone(&routing),
                Arc::clone(&bridge),
                &test.inputs.route_host,
                ROUTE,
                Some(test.pat),
                &parent,
                test.body.clone(),
            )
            .await?;
            anyhow::ensure!(
                successful_value(&response, "session-nested-replay")? == *test.expected_base,
                "explicit fresh PAT did not return the independent stored base result"
            );
        }
        anyhow::ensure!(
            counter(&test, &counter_read).await? == 2,
            "explicit PAT must add exactly one further committed effect"
        );
        assert_counter_trace(&test, &trace, &parent_digest, base_digest, "pat", true)?;
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

async fn diagnose_prior_commit_failure(test: &PriorCommitTest<'_>, trace: &str) {
    let counter = test.project.query_one(
        "SELECT count FROM fresh_only_probe.counter WHERE tenant_id = $1 AND id = $2::text::uuid",
        &[&TENANT, &COUNTER_ID],
    ).await.and_then(|row| row.try_get::<_, i64>(0))
        .map_or_else(|_| json!("read-failed"), |value| json!(value));
    let spans = test.traces.spans();
    let matched: Vec<_> = spans
        .iter()
        .filter(|span| span.span_context.trace_id().to_string() == trace)
        .collect();
    let redactions = (|| -> anyhow::Result<Vec<String>> {
        let registry = serde_json::from_slice(&std::fs::read(&test.inputs.registry_auth_file)?)?;
        diagnostic_redactions(
            &[
                &test.credentials.guest_sql,
                &test.credentials.executor_platform,
                &test.credentials.event_materializer,
                &test.credentials.http_admitter,
                &test.credentials.identity_reader,
                &test.credentials.control_author,
                &test.credentials.management_admitter,
                test.project_url,
                &test.inputs.system_pg_url,
            ],
            &[test.session, test.pat],
            &registry,
        )
    })();
    let Ok(redactions) = redactions else {
        // Never render parser/IO errors: their context may contain credentials.
        eprintln!(
            "FRESH_ONLY_PRIOR_COMMIT_DIAGNOSTIC {}",
            json!({
                "trace": trace, "counter": counter, "matched_spans": matched.len(),
                "details": "redaction-unavailable"
            })
        );
        return;
    };
    let mut details = Vec::new();
    for span in matched {
        let mut errors = Vec::new();
        for event in span.events.iter() {
            for attribute in &event.attributes {
                if attribute.key.as_str() == "error" {
                    errors.push(json!({
                        "event": redact_diagnostic(&event.name, &redactions),
                        "error": redact_diagnostic(&attribute.value.to_string(), &redactions)
                    }));
                }
            }
        }
        details.push(json!({"name": redact_diagnostic(&span.name, &redactions), "errors": errors}));
    }
    eprintln!(
        "FRESH_ONLY_PRIOR_COMMIT_DIAGNOSTIC {}",
        json!({"trace": trace, "counter": counter, "spans": details})
    );
}

fn diagnostic_redactions(
    urls: &[&str],
    tokens: &[&str],
    registry: &Value,
) -> anyhow::Result<Vec<String>> {
    let mut secrets: Vec<String> = tokens.iter().map(|token| (*token).to_owned()).collect();
    for url in urls.iter().filter(|url| !url.is_empty()) {
        secrets.push((*url).to_owned());
        let config: tokio_postgres::Config = url.parse()?;
        if let Some(password) = config.get_password() {
            secrets.push(std::str::from_utf8(password)?.to_owned());
        }
        if let Ok(parsed) = reqwest::Url::parse(url)
            && let Some(encoded) = parsed.password()
        {
            secrets.push(encoded.to_owned());
        }
    }
    // The journey writes only this supported username/password shape. If it
    // grows an encoded auth/token/helper form, suppress diagnostics rather than
    // risk exposing a decoded secret we have not collected.
    let config = registry.as_object().context("registry config shape")?;
    anyhow::ensure!(config.len() == 1, "registry config shape");
    let auths = config
        .get("auths")
        .and_then(Value::as_object)
        .context("registry auths shape")?;
    anyhow::ensure!(!auths.is_empty(), "registry auths missing");
    for entry in auths.values() {
        let entry = entry.as_object().context("registry entry shape")?;
        anyhow::ensure!(entry.len() == 2, "registry entry shape");
        for field in ["username", "password"] {
            let value = entry
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .context("registry field missing")?;
            secrets.push(value.to_owned());
        }
    }
    // Error displays can quote strings. Cover JSON/Rust string escapes too.
    for secret in secrets.clone() {
        let quoted = serde_json::to_string(&secret)?;
        secrets.push(quoted[1..quoted.len() - 1].to_owned());
        let quoted = format!("{secret:?}");
        secrets.push(quoted[1..quoted.len() - 1].to_owned());
    }
    secrets.retain(|secret| !secret.is_empty());
    secrets.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
    secrets.dedup();
    Ok(secrets)
}

fn redact_diagnostic(text: &str, secrets: &[String]) -> String {
    secrets.iter().fold(text.to_owned(), |text, secret| {
        text.replace(secret, "<redacted>")
    })
}

fn assert_counter_trace(
    test: &PriorCommitTest<'_>,
    trace: &str,
    parent_digest: &str,
    base_digest: &str,
    credential: &str,
    reached_base: bool,
) -> anyhow::Result<()> {
    let spans = test.traces.spans();
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
        test.human_id,
    );
    assert_postgres_descendants(&spans, trace, parent);
    for invocation in &invoked {
        anyhow::ensure!(
            span_attribute(invocation, "wamn.caller_credential_kind").as_deref()
                == Some(credential)
                && span_attribute(invocation, "wamn.tenant").as_deref() == Some(TENANT),
            "counter wiring changed the original credential kind or tenant"
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
            test.human_id,
        );
        anyhow::ensure!(
            span_descends_from(&spans, base, parent),
            "base invocation lost parent ancestry"
        );
        assert_postgres_descendants(&spans, trace, base);
    }
    Ok(())
}

async fn counter(test: &PriorCommitTest<'_>, generated_get: &str) -> anyhow::Result<i64> {
    // Exercise the package's actual generated read through the scoped reader,
    // then compare it with an independent owner query over the committed row.
    let (guest, task) = connect(&test.credentials.guest_sql).await?;
    let result = async {
        guest.batch_execute("BEGIN; SET LOCAL search_path = fresh_only_probe, public").await?;
        let row = guest.query_typed_one(generated_get,
            &[(&COUNTER_ID, tokio_postgres::types::Type::TEXT)]).await?;
        let count: i64 = row.try_get("count")?;
        anyhow::ensure!(row.try_get::<_, String>("tenant_id")? == TENANT,
            "generated counter read crossed tenant scope");
        guest.batch_execute("ROLLBACK").await?;
        let independent: i64 = test.project.query_one(
            "SELECT count FROM fresh_only_probe.counter WHERE tenant_id = $1 AND id = $2::text::uuid",
            &[&TENANT, &COUNTER_ID]).await?.get(0);
        anyhow::ensure!(count == independent, "generated and independent counter reads disagree");
        let foreign: i64 = test.project.query_one(
            "SELECT count FROM fresh_only_probe.counter WHERE tenant_id = $1 AND id = $2::text::uuid",
            &[&FOREIGN_TENANT, &FOREIGN_COUNTER_ID]).await?.get(0);
        anyhow::ensure!(foreign == 0, "counter effect changed the foreign tenant row");
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

async fn assert_counter_authority(test: &PriorCommitTest<'_>) -> anyhow::Result<()> {
    let (guest, task) = connect(&test.credentials.guest_sql).await?;
    let result = async {
        let allowed: bool = guest.query_one("SELECT \
            has_column_privilege(current_user, 'fresh_only_probe.counter', 'count', 'UPDATE') \
            AND NOT has_column_privilege(current_user, 'fresh_only_probe.counter', 'id', 'UPDATE') \
            AND NOT has_column_privilege(current_user, 'fresh_only_probe.counter', 'tenant_id', 'UPDATE') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'INSERT') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'DELETE') \
            AND NOT has_table_privilege(current_user, 'fresh_only_probe.counter', 'TRUNCATE')", &[]).await?.get(0);
        anyhow::ensure!(allowed, "fixture counter grants exceed its one effect column");
        let login_bound: bool = guest.query_one("SELECT \
            current_user = session_user AND NOT rolsuper AND NOT rolbypassrls \
            AND current_setting('app.tenant', true) IS NULL \
            AND wamn_authority.current_tenant_key() = wamn_authority.tenant_key($1) \
            FROM pg_roles WHERE rolname = current_user", &[&TENANT]).await?.get(0);
        anyhow::ensure!(login_bound, "counter requires an ordinary tenant-bound GuestSql login");
        guest.batch_execute("BEGIN").await?;
        guest.query_one("SELECT set_config('app.tenant', $1, true)", &[&FOREIGN_TENANT]).await?;
        let rows = guest.query(&format!("{COUNTER_SQL} RETURNING tenant_id, count"), &[]).await?;
        anyhow::ensure!(rows.len() == 1 && rows[0].get::<_, String>(0) == TENANT
            && rows[0].get::<_, i64>(1) == 1,
            "forged GUC changed the counter's login-bound authority");
        let foreign = guest.execute(&format!("{COUNTER_SQL} WHERE tenant_id = $1"),
            &[&FOREIGN_TENANT]).await?;
        guest.batch_execute("ROLLBACK").await?;
        anyhow::ensure!(foreign == 0, "counter update escaped the real GuestSql tenant RLS");
        Ok::<_, anyhow::Error>(())
    }.await;
    task.abort();
    result
}

async fn gate_wiring(test: &PriorCommitTest<'_>, wiring: &Value) -> anyhow::Result<()> {
    let copies = test
        .inputs
        .fresh_only_packages
        .as_ref()
        .context("fresh-only authority copies missing")?;
    let credentials = JourneyCredentials {
        guest_sql: test.credentials.guest_sql.clone(),
        executor_platform: test.credentials.executor_platform.clone(),
        event_materializer: test.credentials.event_materializer.clone(),
        http_admitter: test.credentials.http_admitter.clone(),
        identity_reader: test.credentials.identity_reader.clone(),
        control_author: secret_value(&copies.join("control-author.json"), "url")?,
        management_admitter: secret_value(&copies.join("management-admitter.json"), "url")?,
    };
    let publisher = resolve_subject(test.control, PrincipalKind::Service, test.publisher)
        .await
        .context("resolve the prior-commit management-author principal")?
        .context("the prior-commit management-author principal is absent")?;
    let pat = issue_pat(
        test.control,
        publisher.id(),
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
    let revoked = revoke_pat(test.control, pat.record().prefix()).await;
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

#[cfg(test)]
mod execution_tests {
    use super::{BASE_RECORD_RECEIPT, COUNTER_SQL, OPERATION, parent_component};
    use anyhow::Context as _;
    use wash_runtime::wasmtime::component::{Component, Linker, TypedFunc};
    use wash_runtime::wasmtime::{Engine, Store};

    mod node_bindings {
        wash_runtime::wasmtime::component::bindgen!({
            path: "../../../crates/execution/router/wit",
            world: "node",
            additional_derives: [PartialEq],
            wasmtime_crate: wash_runtime::wasmtime,
        });
    }

    mod postgres_bindings {
        wash_runtime::wasmtime::component::bindgen!({
            path: "../../../crates/platform/runtime/wit",
            world: "postgres-plugin",
            wasmtime_crate: wash_runtime::wasmtime,
        });
    }

    use node_bindings::wamn::node::types::{Emission, NodeContext, NodeError};
    use postgres_bindings::wamn::postgres::types::{PgError, SqlValue};

    #[derive(Default)]
    struct Calls {
        order: Vec<&'static str>,
        nested: Option<(NodeContext, String)>,
    }

    #[tokio::test]
    #[ignore = "requires WAMN_TENANT_KEY_PG_URL on a fresh disposable PostgreSQL 18 server"]
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
        let url = std::env::var("WAMN_TENANT_KEY_PG_URL")
            .context("WAMN_TENANT_KEY_PG_URL must arm this disposable test")?;
        let config: tokio_postgres::Config = url
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
                tenant: super::TENANT,
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
                &[&super::TENANT, &super::FOREIGN_TENANT],
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
                &[&role, &super::TENANT],
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
                let expected_count = if tenant == super::TENANT {
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

    #[test]
    fn prior_commit_diagnostic_redacts_fixture_credentials() -> anyhow::Result<()> {
        let urls = [
            "postgresql://fixture:encoded%21password@localhost/system",
            "postgresql://fixture:other-password@localhost/project",
        ];
        let tokens = ["fixture-pat-full-token", "fixture-session-full-token"];
        let registry = serde_json::json!({"auths": {
            "registry-a": {"username": "fixture-user", "password": "registry-password"},
            "registry-b": {"username": "second-user", "password": "quoted\"password"}
        }});
        let secrets = super::diagnostic_redactions(&urls, &tokens, &registry)?;
        for value in urls.into_iter().chain(tokens).chain([
            "encoded!password",
            "encoded%21password",
            "other-password",
            "fixture-user",
            "registry-password",
            "second-user",
            "quoted\"password",
            "quoted\\\"password",
        ]) {
            assert_eq!(super::redact_diagnostic(value, &secrets), "<redacted>");
        }
        assert_eq!(
            super::redact_diagnostic("unreachable instruction", &secrets),
            "unreachable instruction"
        );
        assert!(super::diagnostic_redactions(&["not-a-database-url"], &tokens, &registry).is_err());
        assert!(
            super::diagnostic_redactions(
                &urls,
                &tokens,
                &serde_json::json!({"auths": {"registry": {"auth": "opaque-auth"}}})
            )
            .is_err()
        );
        Ok(())
    }

    /// Isolate the WAT's memory ABI from DB/authorization setup. The deployed
    /// test above remains the witness for actual commits and fresh-only checks.
    #[tokio::test]
    async fn counter_parent_executes_sql_then_forwards_the_exact_nested_call() -> anyhow::Result<()>
    {
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
        let input = "[{\"request_id\":\"abi-only\",\"value\":17}]";
        for refuse_nested in [false, true] {
            let mut linker = Linker::<Calls>::new(&engine);
            linker.instance("wamn:node/types@0.1.0")?;
            linker.instance("wamn:postgres/types@0.1.0")?;
            linker
                .instance("wamn:postgres/client@0.1.0")?
                .func_wrap_async(
                    "execute",
                    |mut store, (sql, params): (String, Vec<SqlValue>)| {
                        Box::new(async move {
                            assert_eq!(sql, COUNTER_SQL);
                            assert!(params.is_empty());
                            store.data_mut().order.push("sql");
                            Ok((Ok::<u64, PgError>(1),))
                        })
                    },
                )?;
            linker.instance(BASE_RECORD_RECEIPT)?.func_wrap_async(
                "run",
                move |mut store, (context, input): (NodeContext, String)| {
                    Box::new(async move {
                        store.data_mut().order.push("nested");
                        store.data_mut().nested = Some((context, input.clone()));
                        if refuse_nested {
                            return Err(wash_runtime::wasmtime::Error::msg(
                                "local nested refusal sentinel",
                            ));
                        }
                        Ok((Ok::<Emission, NodeError>(Emission {
                            payload: input,
                            port: Some("main".to_owned()),
                        }),))
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
            let run: TypedFunc<(&NodeContext, &str), (Result<Emission, NodeError>,)> =
                instance.get_typed_func(&mut store, &export)?;
            let result = run.call_async(&mut store, (&context, input)).await;
            if refuse_nested {
                let error = result.expect_err("the nested host error must propagate");
                anyhow::ensure!(
                    format!("{error:#}").contains("local nested refusal sentinel"),
                    "the fixture trapped before reaching the nested refusal: {error:#}"
                );
            } else {
                let (result,) = result
                    .map_err(anyhow::Error::from)
                    .context("execute the actual counter-parent WAT")?;
                assert_eq!(
                    result,
                    Ok(Emission {
                        payload: input.to_owned(),
                        port: Some("main".to_owned())
                    })
                );
            }
            assert_eq!(store.data().order, ["sql", "nested"]);
            assert_eq!(
                store.data().nested,
                Some((context.clone(), input.to_owned()))
            );
        }
        Ok(())
    }
}
