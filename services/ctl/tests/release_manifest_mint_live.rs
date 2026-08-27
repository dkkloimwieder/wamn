//! Live proof that serving-manifest v2 is minted from current catalog facts.
//!
//! Set `WAMN_RELEASE_MANIFEST_MINT_PG_URL` to a disposable PostgreSQL 18
//! database. The live tests drop and recreate its `catalog` schema, so each one
//! holds the shared control live-database lock for the whole of its run.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AttachmentKind, CatalogIdentityError, DefinitionHash, ManifestDigest,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingManifest, ServingRegistration,
    ServingRegistrationInput, ServingRelease, ServingWiring, WiringDocument, WiringEventOperation,
    WiringNode, WiringTerminal,
};
use wamn_ctl::author_wiring::{
    AuthorWiringArgs, AuthorWiringErrorKind, AuthorWiringRequest, author_wiring,
};
use wamn_ctl::publish_catalog::{CATALOG_PLANE_RESIDENCY_REFUSAL, ensure_catalog_storage};
use wamn_ctl::publish_release::{
    DeploymentCoordinate, MintManifestError, MintManifestErrorKind, MintReleaseManifest,
    PublishReleaseArgs, RELEASE_MANIFEST_MINT_REFUSAL, ReleaseWiringTarget, attest_deployment,
    mint_release_manifest, project_release_identity,
};
use wamn_ctl::push_release_manifest::{
    PushReleaseManifestArgs, ReleaseManifestPublishDisposition, publish_release_manifest,
};
use wamn_execution_contract::EntryKind;
use wamn_schema_control::attestation::{AttestationError, AttestationErrorKind};
use wamn_schema_control::{
    Attachment as ExposureAttachment, AttachmentKind as ExposureAttachmentKind, ExposureRelease,
    FlowExposure, HttpRoute, Source, SourceKind, resolve_exposure,
};

const TENANT: &str = "manifest-mint-tenant";
const CATALOG: &str = "manifest-mint-catalog";
const CATALOG_VERSION: i32 = 3;
const ENVIRONMENT: &str = "prod";
const ORG: &str = "manifest-mint-org";
const PROJECT: &str = "manifest-mint-project";
const COMPONENT_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const COMPONENT_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const WRONG_GRAPH: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const DEFINITION: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
/// The deployed control-plane DDL, read for its gate-report declaration alone.
const CONTROL_PORTABLE_STORE: &str = include_str!("../../../deploy/sql/control-portable-store.sql");
/// The deployed run-plane DDL, read for the provisioned environment fact alone.
const RUN_STATE_SCHEMA: &str = include_str!("../../../deploy/sql/run-state.sql");
/// The run-plane schema of record, named on the publish surface like any deployed one.
const RUN_SCHEMA: &str = "wamn_run";
const FACT_FINGERPRINT: &str =
    "sha256:6666666666666666666666666666666666666666666666666666666666666666";

#[test]
fn resolved_exposure_hash_round_trips_through_serving_manifest_admission() {
    let exposure = ExposureRelease {
        sources: vec![Source {
            id: "public".to_string(),
            kind: SourceKind::Auth,
            definition: json!({"mode": "none"}),
        }],
        attachments: vec![ExposureAttachment {
            id: "orders-http".to_string(),
            kind: ExposureAttachmentKind::Http,
            flow_id: "orders".to_string(),
            source_id: "public".to_string(),
            route: Some(HttpRoute {
                host: "api.example.test".to_string(),
                path: "/orders".to_string(),
                method: "POST".to_string(),
            }),
            mappings: Vec::new(),
            run_deadline_ms: 30_000,
            response_deadline_ms: Some(10_000),
        }],
    };
    let resolved = resolve_exposure(
        &exposure,
        &[FlowExposure {
            flow_id: "orders",
            entry_kind: EntryKind::Request,
            artifact_hash: COMPONENT_A,
        }],
    )
    .expect("the exposure boundary resolves one attachment")
    .pop()
    .expect("one attachment was authored");
    let admitted_hash = DefinitionHash::parse(resolved.definition_hash.clone())
        .expect("the exposure mint uses the catalog digest spelling");
    assert_eq!(admitted_hash.as_str(), resolved.definition_hash);

    let manifest = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            tenant_id: TENANT.to_string(),
            catalog_id: CATALOG.to_string(),
            catalog_version: CATALOG_VERSION as u32,
            environment: ENVIRONMENT.to_string(),
        },
        components: BTreeSet::new(),
        wirings: BTreeSet::from([ServingWiring {
            wiring_id: "orders".to_string(),
            wiring_version: 1,
            graph_hash: DefinitionHash::parse(WRONG_GRAPH)
                .expect("fixture definition hash is canonical"),
        }]),
        attachments: BTreeMap::from([(
            "orders-http".to_string(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                wiring_id: "orders".to_string(),
                wiring_version: 1,
                definition_hash: admitted_hash.clone(),
                definition: json!({"id": "orders-http", "kind": "http"}),
                auth_policy: json!({"mode": "none"}),
            },
        )]),
        registrations: BTreeMap::new(),
    };
    let (admitted, _) = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())
        .expect("the serving-manifest boundary admits the minted definition hash");
    assert_eq!(
        admitted.attachments["orders-http"].definition_hash.as_str(),
        resolved.definition_hash
    );

    let mut bare_document = serde_json::to_value(manifest).expect("manifest serializes");
    bare_document["attachments"]["orders-http"]["definition-hash"] = json!(
        admitted_hash
            .as_str()
            .strip_prefix("sha256:")
            .expect("admitted digest prefix")
    );
    let bare_bytes = wamn_execution_contract::canonical_json_bytes(&bare_document);
    let error = ServingManifest::from_canonical_bytes(&bare_bytes)
        .expect_err("a bare definition hash cannot deserialize into the manifest");
    assert!(matches!(
        error,
        CatalogIdentityError::InvalidDefinition { ref message }
            if message.contains("definition-hash")
    ));
}

#[test]
fn release_membership_is_exact_wiring_to_admitted_digest() {
    const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
    let table = CATALOG_SCHEMA
        .split("CREATE TABLE catalog.release_components (")
        .nth(1)
        .and_then(|tail| tail.split("\n);").next())
        .expect("release component membership table");
    for required in [
        "wiring_id        text NOT NULL",
        "wiring_version   int NOT NULL",
        "component_digest text NOT NULL",
        "REFERENCES catalog.releases",
        "REFERENCES catalog.wirings",
        "REFERENCES catalog.component_library",
    ] {
        assert!(table.contains(required), "missing {required:?}");
    }
    assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER release_components_immutable"));
    assert!(CATALOG_SCHEMA.contains("BEFORE UPDATE OR DELETE ON catalog.release_components"));
    assert!(CATALOG_SCHEMA.contains("CREATE TABLE catalog.release_manifest_v2_snapshots"));
    assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER release_manifest_v2_snapshots_immutable"));
    assert!(CATALOG_SCHEMA.contains("CONSTRAINT release_manifest_v2_snapshots_exact_hash CHECK"));
    assert!(
        CATALOG_SCHEMA
            .contains("manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex')")
    );
    assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER release_components_snapshot_seal"));
    assert!(CATALOG_SCHEMA.contains("MESSAGE = 'release-component-membership-frozen'"));
}

// wamn-hopk R5: this test read services/ctl/src/publish_release.rs as text and
// grepped it for retired tokens. Deleted. The mint's behaviour is proven by the
// live arms in this file, which actually mint against a database.

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the catalog database");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

async fn provision(admin: &Client) {
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .expect("reset the catalog schema and prerequisite role");
    ensure_catalog_storage(admin)
        .await
        .expect("install the production catalog schema");
}

/// One contiguous run of statements lifted verbatim out of a schema of record.
fn statements_of_record(source: &'static str, first: &str, last: &str) -> &'static str {
    let start = source
        .find(first)
        .unwrap_or_else(|| panic!("the schema of record no longer declares {first:?}"));
    let end = source[start..]
        .find(last)
        .unwrap_or_else(|| panic!("the schema of record no longer declares {last:?}"))
        + last.len();
    &source[start..start + end]
}

/// Install the provisioned environment fact exactly as `run-state.sql` declares
/// it — relation, forced row security, tenant policy and grants — so the live
/// proof reads the relation production reads rather than a restatement of it.
///
/// `expected_environment` of `None` installs the relation with NO row for this
/// tenant: the absent case is a real empty relation, not a dropped table.
async fn provision_environment_policy(admin: &Client, expected_environment: Option<&str>) {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {RUN_SCHEMA} CASCADE; \
             DO $$ BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             {} \
             {}",
            statements_of_record(
                RUN_STATE_SCHEMA,
                "CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;",
                "GRANT USAGE ON SCHEMA wamn_run TO wamn_effect_writer;",
            ),
            statements_of_record(
                RUN_STATE_SCHEMA,
                "CREATE TABLE wamn_run.environment_policies (",
                "GRANT SELECT ON TABLE wamn_run.environment_policies TO wamn_scenario_author;",
            ),
        ))
        .await
        .expect("install the provisioned environment relation of record");
    if let Some(expected_environment) = expected_environment {
        admin
            .execute(
                "INSERT INTO wamn_run.environment_policies \
                       (tenant_id, expected_environment, durability_class) \
                 VALUES ($1, $2, 'standard')",
                &[&TENANT, &expected_environment],
            )
            .await
            .expect("project the provisioned environment fact");
    }
}

/// Assert that this release left no frozen closure behind.
async fn assert_no_release_was_frozen(admin: &Client) {
    for relation in [
        "catalog.release_manifest_v2_snapshots",
        "catalog.release_components",
    ] {
        let frozen: i64 = admin
            .query_one(
                &format!(
                    "SELECT count(*) FROM {relation} \
                      WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3"
                ),
                &[&TENANT, &CATALOG, &CATALOG_VERSION],
            )
            .await
            .expect("count the frozen release rows")
            .get(0);
        assert_eq!(frozen, 0, "a refused publish committed rows to {relation}");
    }
}

/// Mint one first release through the CLI verb against a seeded environment.
async fn publish_first_release(
    url: &str,
    control_url: &str,
    documents: &Path,
) -> anyhow::Result<()> {
    wamn_ctl::publish_release::run(PublishReleaseArgs {
        database_url: url.to_owned(),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        tenant: TENANT.to_string(),
        catalog_id: CATALOG.to_string(),
        catalog_version: CATALOG_VERSION as u32,
        run_schema: RUN_SCHEMA.to_string(),
        wirings: targets().into_iter().collect(),
        attachments: write_document(documents, "attachments.json", &attachments()),
        registrations: write_document(documents, "registrations.json", &registrations()),
        control_database_url: control_url.to_owned(),
    })
    .await
}

/// The CONTROL database this fixture creates beside the project one.
///
/// The two planes are two DATABASES that share relation names on purpose
/// (`deploy/sql/control-portable-store.sql`: "database residency, not a renamed
/// schema, distinguishes them"), so a fixture that installed the control store
/// into the project database would prove nothing about the crossing.
const CONTROL_DATABASE: &str = "wamn_release_attestation_control";

/// Point a live-test URL at another database on the SAME cluster.
fn sibling_url(url: &str, database: &str) -> String {
    let mut sibling = url::Url::parse(url).expect("the live database URL parses");
    sibling.set_path(database);
    sibling.to_string()
}

/// Create the control database beside the project one and install the PRODUCTION
/// control portable store into it.
///
/// `CREATE DATABASE` and `DROP DATABASE` each go over the simple protocol as the
/// only statement of their request, because neither may run inside a transaction
/// block.
async fn provision_control_plane(url: &str) -> String {
    let (maintenance, maintenance_task) = connect(&sibling_url(url, "postgres")).await;
    maintenance
        .batch_execute(
            "DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY \
                 ARRAY['wamn_system', 'wamn_control_author', 'wamn_app', 'wamn_scenario_author'] \
               LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$;",
        )
        .await
        .expect("bootstrap the cluster-global roles the control store grants to");
    maintenance
        .simple_query(&format!(
            "DROP DATABASE IF EXISTS {CONTROL_DATABASE} WITH (FORCE)"
        ))
        .await
        .expect("retire any control database a previous run left behind");
    maintenance
        .simple_query(&format!("CREATE DATABASE {CONTROL_DATABASE}"))
        .await
        .expect("create the control database beside the project one");
    drop(maintenance);
    let _ = maintenance_task.await;

    let control_url = sibling_url(url, CONTROL_DATABASE);
    let (control, control_task) = connect(&control_url).await;
    // The store's own authority self-check refuses to apply while the project
    // plane's author role can reach this database at all.
    control
        .batch_execute(&format!(
            "DO $$ BEGIN \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', \
                              pg_catalog.current_database()); \
             END $$; \
             {CONTROL_PORTABLE_STORE}"
        ))
        .await
        .expect("install the production control portable store");
    drop(control);
    let _ = control_task.await;
    control_url
}

/// The tables a schema actually holds, read off the server.
async fn tables(client: &Client, schema: &str) -> Vec<String> {
    client
        .query(
            "SELECT tablename::text FROM pg_catalog.pg_tables \
             WHERE schemaname = $1 ORDER BY tablename",
            &[&schema],
        )
        .await
        .expect("read the schema inventory")
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

/// A registry nothing can be listening on, and the credential a verb must load
/// before it can even try to reach one.
///
/// Port 1 is privileged and unbound: a connection to it is refused rather than
/// answered, so a verb pointed here gets no further than the transport.
const UNREACHABLE_REGISTRY: &str = "127.0.0.1:1";

fn unreachable_registry_credential(documents: &Path) -> PathBuf {
    let path = documents.join("registry-auth.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"auths":{{"{UNREACHABLE_REGISTRY}":{{"username":"live","password":"live"}}}}}}"#
        ),
    )
    .expect("write the registry credential");
    path
}

/// What the control plane holds for this release: catalog header, release
/// identity, and attestation counts, read off the server.
async fn control_state(control: &Client) -> (i64, i64, i64) {
    let row = control
        .query_one(
            "SELECT (SELECT count(*) FROM catalog.catalogs \
                      WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3), \
                    (SELECT count(*) FROM catalog.releases \
                      WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3), \
                    (SELECT count(*) FROM catalog.deployment_attestations \
                      WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3)",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the control plane's release state");
    (row.get(0), row.get(1), row.get(2))
}

/// The one attestation row's content, if there is exactly one.
async fn attested_content(control: &Client) -> Option<(String, String)> {
    let rows = control
        .query(
            "SELECT deployed_manifest_hash, attested_at::text \
               FROM catalog.deployment_attestations \
              WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the attested content");
    assert!(
        rows.len() <= 1,
        "the coordinate is attested {} times",
        rows.len()
    );
    rows.first().map(|row| (row.get(0), row.get(1)))
}

/// Seed the whole closure one CLI publish needs, short of its environment policy.
async fn seed_publishable_release(admin: &Client, url: &str, documents: &Path) {
    provision(admin).await;
    provision_gate_reports(admin).await;
    seed_release(admin, CATALOG_VERSION, "applied").await;
    seed_component(admin, "http-request", COMPONENT_A).await;
    seed_component(admin, "transform", COMPONENT_B).await;
    let orders = wiring("orders", 1, "http-request", WiringTerminal::Respond);
    let shipping = wiring(
        "shipping",
        2,
        "transform",
        WiringTerminal::emit("orders", WiringEventOperation::Insert),
    );
    for document in [&orders, &shipping] {
        record_green_report(admin, document).await;
        wamn_ctl::author_wiring::run(AuthorWiringArgs {
            database_url: url.to_owned(),
            control_database_url: url.to_owned(),
            tenant: TENANT.to_string(),
            catalog_id: CATALOG.to_string(),
            gated_catalog_version: CATALOG_VERSION as u32,
            wiring_document: write_document(
                documents,
                &format!("{}.json", document.wiring_id),
                document,
            ),
        })
        .await
        .expect("the authorship verb gates and stores one wiring document");
    }
}

async fn seed_release(admin: &Client, catalog_version: i32, state: &str) {
    admin
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the seed session");
    admin
        .execute(
            "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ($1, $2, $3, $4, '0.1', $5)",
            &[&TENANT, &CATALOG, &catalog_version, &ENVIRONMENT, &state],
        )
        .await
        .expect("seed the catalog version");
    admin
        .execute(
            "INSERT INTO catalog.releases \
                   (tenant_id, catalog_id, catalog_version) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &CATALOG, &catalog_version],
        )
        .await
        .expect("seed the release identity");
}

async fn seed_component(admin: &Client, name: &str, digest: &str) {
    admin
        .execute(
            "INSERT INTO catalog.component_library \
                   (tenant_id, catalog_id, catalog_version, component, interface_version, \
                    operation, component_digest, imports, imports_fingerprint, effects, \
                    input_ports, output_ports, parameters) \
             VALUES ($1, $2, $3, $4, '0.1', 'call', $5, \
                     '[]'::jsonb, $6, '[]'::jsonb, '[]'::jsonb, '[]'::jsonb, '[]'::jsonb)",
            &[
                &TENANT,
                &CATALOG,
                &CATALOG_VERSION,
                &name,
                &digest,
                &FACT_FINGERPRINT,
            ],
        )
        .await
        .expect("seed an admitted component fact");
}

fn wiring(id: &str, version: u32, component: &str, terminal: WiringTerminal) -> WiringDocument {
    WiringDocument::new(
        id,
        version,
        "node",
        BTreeMap::from([(
            "node".to_string(),
            WiringNode {
                component: component.to_string(),
                interface_version: "0.1".to_string(),
                operation: "call".to_string(),
                params: BTreeMap::new(),
                terminal: Some(terminal),
            },
        )]),
        Vec::new(),
        Vec::new(),
    )
    .expect("fixture wiring is structurally valid")
}

/// Install ONLY the gate-report relation the authorship verb reads.
///
/// The report is a CONTROL-plane fact and `catalog.wirings` a PROJECT-plane one,
/// so production reaches two databases for it. This fixture co-locates them in
/// the one disposable database this test owns, because its subject is the mint,
/// not the residency; the relation is created from the deployed DDL's own text,
/// and the two-database proof of the authorship guard itself lives in
/// `author_wiring_gate_report_live.rs`.
async fn provision_gate_reports(admin: &Client) {
    let declaration = CONTROL_PORTABLE_STORE
        .split_once("CREATE TABLE IF NOT EXISTS wamn_run.gate_reports (")
        .expect("the control portable store declares the gate-report relation")
        .1
        .split_once("\n);")
        .expect("the gate-report declaration is terminated")
        .0;
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             CREATE SCHEMA wamn_run; \
             CREATE TABLE wamn_run.gate_reports ({declaration});"
        ))
        .await
        .expect("install the gate-report relation authorship reads");
}

/// Record the green verdict one document must carry before it can be authored.
async fn record_green_report(control: &Client, document: &WiringDocument) {
    let hash = document.wiring_hash();
    control
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, true, '{}'::jsonb) ON CONFLICT DO NOTHING",
            &[&TENANT, &hash.as_str()],
        )
        .await
        .expect("record the document's green gate verdict");
}

/// Author one gated wiring through the production authorship verb.
///
/// `seed_wiring`'s hand SQL retired with wamn-1xb5: the fixtures are documents
/// now, admitted by exactly the predicates a first release is authored under.
/// One of those predicates is now a GREEN gate report at the document's own
/// hash, which is why the fixture records one first: an unreported document is
/// unauthorable, so no released wiring can exist without a passing gate.
async fn author(control: &Client, admin: &mut Client, document: &WiringDocument) -> DefinitionHash {
    record_green_report(control, document).await;
    let transaction = admin
        .transaction()
        .await
        .expect("open the wiring authorship transaction");
    let hash = author_wiring(
        control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            catalog_id: CATALOG,
            gated_catalog_version: CATALOG_VERSION,
            document,
        },
    )
    .await
    .expect("the authorship verb gates and stores the wiring");
    transaction
        .commit()
        .await
        .expect("commit the wiring authorship");
    hash
}

/// Store one `catalog.wirings` row at a caller-chosen hash with hand SQL.
///
/// Every state this writes is UNREACHABLE through `author-wiring`, which gates
/// the document and derives the stored hash from what it writes, so each one
/// the mint must refuse is injected on purpose: a hash that does not name its
/// own document, and a document naming an unadmitted component.
async fn inject_stored_wiring(admin: &Client, document: &WiringDocument, graph_hash: &str) {
    let version = i32::try_from(document.version).expect("fixture version fits storage");
    let graph = serde_json::to_string(document).expect("wiring serializes");
    admin
        .execute(
            "INSERT INTO catalog.wirings \
                   (tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
                    graph_json, wiring_hash) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7)",
            &[
                &TENANT,
                &CATALOG,
                &document.wiring_id,
                &version,
                &CATALOG_VERSION,
                &graph,
                &graph_hash,
            ],
        )
        .await
        .expect("inject a wiring row the authorship verb would refuse");
}

fn targets() -> BTreeSet<ReleaseWiringTarget> {
    BTreeSet::from([
        ReleaseWiringTarget {
            wiring_id: "orders".to_string(),
            wiring_version: 1,
        },
        ReleaseWiringTarget {
            wiring_id: "shipping".to_string(),
            wiring_version: 2,
        },
    ])
}

fn attachments() -> BTreeMap<String, ServingAttachment> {
    BTreeMap::from([(
        "orders-http".to_string(),
        ServingAttachment {
            kind: AttachmentKind::Http,
            wiring_id: "orders".to_string(),
            wiring_version: 1,
            definition_hash: DefinitionHash::parse(DEFINITION)
                .expect("fixture definition hash is canonical"),
            definition: json!({
                "id": "orders-http",
                "kind": "http",
                "run-deadline-ms": 30000
            }),
            auth_policy: json!({"mode": "none"}),
        },
    )])
}

fn registrations() -> BTreeMap<String, ServingRegistration> {
    BTreeMap::from([(
        "orders-changed".to_string(),
        ServingRegistration {
            wiring_id: "shipping".to_string(),
            wiring_version: 2,
            entity: "orders".to_string(),
            ops: BTreeSet::from(["insert".to_string(), "update".to_string()]),
            input: ServingRegistrationInput::Batch,
        },
    )])
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn current_component_and_wiring_facts_freeze_one_v2_manifest() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");
    let (mut admin, task) = connect(&url).await;
    // The authorship verb reads the gate report on its own connection, so the
    // fixture holds a second one; a `Transaction` borrows `admin` exclusively.
    let (control, control_task) = connect(&url).await;
    provision(&admin).await;
    provision_gate_reports(&admin).await;
    seed_release(&admin, CATALOG_VERSION, "applied").await;
    seed_component(&admin, "http-request", COMPONENT_A).await;
    seed_component(&admin, "transform", COMPONENT_B).await;
    let orders = wiring("orders", 1, "http-request", WiringTerminal::Respond);
    let shipping = wiring(
        "shipping",
        2,
        "transform",
        WiringTerminal::emit("orders", WiringEventOperation::Insert),
    );
    // The authorship verb derives the stored hash from the document it stores,
    // so the fixture's own hash is what a gated wiring row must carry.
    let orders_hash = author(&control, &mut admin, &orders).await;
    let shipping_hash = author(&control, &mut admin, &shipping).await;
    assert_eq!(orders_hash, orders.wiring_hash());
    assert_eq!(shipping_hash, shipping.wiring_hash());

    // Authorship is one idempotent act. An exact resubmission converges, while
    // a second document at one authored coordinate refuses: the stored
    // definition is immutable, so there is nothing to replace it with.
    assert_eq!(author(&control, &mut admin, &orders).await, orders_hash);
    let rewired = wiring("orders", 1, "transform", WiringTerminal::Respond);
    // Green-reported on its own terms, so the coordinate conflict below is what
    // refuses it — not the report guard standing in front of it.
    record_green_report(&control, &rewired).await;
    let transaction = admin
        .transaction()
        .await
        .expect("open the conflicting authorship transaction");
    let refusal = author_wiring(
        &control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            catalog_id: CATALOG,
            gated_catalog_version: CATALOG_VERSION,
            document: &rewired,
        },
    )
    .await
    .expect_err("a second document at one authored coordinate refuses");
    assert_eq!(refusal.kind(), AuthorWiringErrorKind::Conflict);
    transaction
        .rollback()
        .await
        .expect("close the authorship conflict");

    // The verb does not merely sit next to the compatibility gate, it CALLS it:
    // a document whose node names a component with no admitted fact in the gate
    // scope refuses as `Gate`, and nothing reaches `catalog.wirings`. Deriving
    // the stored hash from the document instead of from the gate's return
    // writes this row, so this arm is what pins the call site. No report is
    // recorded for it, and none is needed: the compatibility gate runs first,
    // so `Gate` is still the predicate that names the fault.
    let ungated = wiring("ungated", 4, "unadmitted", WiringTerminal::Respond);
    let transaction = admin
        .transaction()
        .await
        .expect("open the ungated authorship transaction");
    let refusal = author_wiring(
        &control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            catalog_id: CATALOG,
            gated_catalog_version: CATALOG_VERSION,
            document: &ungated,
        },
    )
    .await
    .expect_err("a wiring naming no admitted component refuses at the gate");
    assert_eq!(refusal.kind(), AuthorWiringErrorKind::Gate);
    let ungated_rows: i64 = transaction
        .query_one(
            "SELECT count(*) FROM catalog.wirings \
              WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = $3",
            &[&TENANT, &CATALOG, &ungated.wiring_id],
        )
        .await
        .expect("count the ungated wiring rows")
        .get(0);
    assert_eq!(
        ungated_rows, 0,
        "a wiring the gate refuses must not reach catalog.wirings"
    );
    transaction
        .rollback()
        .await
        .expect("close the gate refusal");

    let corrupt = wiring(
        "corrupt",
        3,
        "transform",
        WiringTerminal::emit("orders", WiringEventOperation::Insert),
    );
    inject_stored_wiring(&admin, &corrupt, WRONG_GRAPH).await;
    let corrupt_targets = BTreeSet::from([ReleaseWiringTarget {
        wiring_id: "corrupt".to_string(),
        wiring_version: 3,
    }]);
    let empty_attachments = BTreeMap::new();
    let empty_registrations = BTreeMap::new();
    let corrupt_request = MintReleaseManifest {
        tenant_id: TENANT,
        catalog_id: CATALOG,
        catalog_version: CATALOG_VERSION,
        wirings: &corrupt_targets,
        attachments: &empty_attachments,
        registrations: &empty_registrations,
    };
    let transaction = admin
        .transaction()
        .await
        .expect("open the corrupt-hash transaction");
    let refusal = mint_release_manifest(&transaction, &corrupt_request)
        .await
        .expect_err("a stored hash that does not name its wiring document refuses");
    assert_eq!(refusal.kind(), MintManifestErrorKind::Wiring);
    transaction
        .rollback()
        .await
        .expect("close the corrupt-hash refusal");

    let wiring_targets = targets();
    let release_attachments = attachments();
    let release_registrations = registrations();
    let request = MintReleaseManifest {
        tenant_id: TENANT,
        catalog_id: CATALOG,
        catalog_version: CATALOG_VERSION,
        wirings: &wiring_targets,
        attachments: &release_attachments,
        registrations: &release_registrations,
    };

    let transaction = admin
        .transaction()
        .await
        .expect("open the mint transaction");
    let minted = mint_release_manifest(&transaction, &request)
        .await
        .expect("current admitted facts mint a v2 release");
    transaction
        .commit()
        .await
        .expect("commit the closure freeze");
    assert_eq!(
        ServingManifest::from_canonical_bytes(&minted.canonical_bytes),
        Ok((minted.manifest.clone(), minted.digest.clone()))
    );
    assert_eq!(
        minted.manifest.wirings,
        BTreeSet::from([
            ServingWiring {
                wiring_id: "orders".to_string(),
                wiring_version: 1,
                graph_hash: orders_hash,
            },
            ServingWiring {
                wiring_id: "shipping".to_string(),
                wiring_version: 2,
                graph_hash: shipping_hash,
            },
        ])
    );

    let frozen: i64 = admin
        .query_one(
            "SELECT count(*) FROM catalog.release_components \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("count the frozen membership")
        .get(0);
    assert_eq!(frozen, 2);
    let snapshot = admin
        .query_one(
            "SELECT manifest_digest, canonical_bytes \
             FROM catalog.release_manifest_v2_snapshots \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the complete v2 freeze");
    assert_eq!(snapshot.get::<_, String>(0), minted.digest.as_str());
    assert_eq!(snapshot.get::<_, Vec<u8>>(1), minted.canonical_bytes);

    let sealed = admin
        .execute(
            "INSERT INTO catalog.release_components \
                   (tenant_id, catalog_id, catalog_version, wiring_id, wiring_version, \
                    component_digest) \
             VALUES ($1, $2, $3, 'corrupt', 3, $4)",
            &[&TENANT, &CATALOG, &CATALOG_VERSION, &COMPONENT_B],
        )
        .await
        .expect_err("membership insertion after the complete snapshot refuses");
    let database = sealed
        .as_db_error()
        .expect("the membership seal is a PostgreSQL refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(database.message(), "release-component-membership-frozen");

    let other_version = CATALOG_VERSION + 1;
    seed_release(&admin, other_version, "draft").await;
    let wrong_digest = admin
        .execute(
            "INSERT INTO catalog.release_manifest_v2_snapshots \
                   (tenant_id, catalog_id, catalog_version, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &TENANT,
                &CATALOG,
                &other_version,
                &WRONG_GRAPH,
                &minted.canonical_bytes,
            ],
        )
        .await
        .expect_err("a snapshot digest that does not name its exact bytes refuses");
    let database = wrong_digest
        .as_db_error()
        .expect("the digest weld is a PostgreSQL refusal");
    assert_eq!(database.code(), &SqlState::CHECK_VIOLATION);
    assert_eq!(
        database.constraint(),
        Some("release_manifest_v2_snapshots_exact_hash")
    );

    let transaction = admin
        .transaction()
        .await
        .expect("open the retry transaction");
    let retried = mint_release_manifest(&transaction, &request)
        .await
        .expect("an exact retry converges");
    transaction.commit().await.expect("commit the exact retry");
    assert_eq!(retried.canonical_bytes, minted.canonical_bytes);

    let mut changed_attachments = attachments();
    changed_attachments
        .get_mut("orders-http")
        .expect("fixture attachment")
        .definition = json!({
        "id": "orders-http",
        "kind": "http",
        "run-deadline-ms": 1000
    });
    let attachment_drift = MintReleaseManifest {
        attachments: &changed_attachments,
        ..request
    };
    let transaction = admin
        .transaction()
        .await
        .expect("open the attachment-drift transaction");
    let refusal = mint_release_manifest(&transaction, &attachment_drift)
        .await
        .expect_err("a frozen release cannot change attachment facts");
    assert_eq!(refusal.kind(), MintManifestErrorKind::ClosureConflict);
    transaction
        .rollback()
        .await
        .expect("close the attachment-drift refusal");

    let mut changed_registrations = registrations();
    changed_registrations
        .get_mut("orders-changed")
        .expect("fixture registration")
        .ops
        .insert("delete".to_string());
    let registration_drift = MintReleaseManifest {
        registrations: &changed_registrations,
        ..request
    };
    let transaction = admin
        .transaction()
        .await
        .expect("open the registration-drift transaction");
    let refusal = mint_release_manifest(&transaction, &registration_drift)
        .await
        .expect_err("a frozen release cannot change registration facts");
    assert_eq!(refusal.kind(), MintManifestErrorKind::ClosureConflict);
    transaction
        .rollback()
        .await
        .expect("close the registration-drift refusal");

    let one_wiring = BTreeSet::from([ReleaseWiringTarget {
        wiring_id: "orders".to_string(),
        wiring_version: 1,
    }]);
    let conflicting = MintReleaseManifest {
        wirings: &one_wiring,
        attachments: &BTreeMap::new(),
        registrations: &BTreeMap::new(),
        ..request
    };
    let transaction = admin
        .transaction()
        .await
        .expect("open the conflict transaction");
    let refusal = mint_release_manifest(&transaction, &conflicting)
        .await
        .expect_err("a frozen release cannot lose a wiring");
    assert_eq!(refusal.kind(), MintManifestErrorKind::ClosureConflict);
    transaction.rollback().await.expect("close the refusal");

    drop(admin);
    drop(control);
    let _ = task.await;
    let _ = control_task.await;
}

/// The mint does not merely sit next to the wiring/component compatibility
/// gate, it CALLS it.
///
/// `author-wiring` refuses a document naming a component with no admitted fact,
/// and `catalog.component_library` is insert-only, so a fact cannot be withdrawn
/// from under a gated wiring either: the row is injected with hand SQL. Dropping
/// the gate call does not merely admit a bad wiring, it turns this typed refusal
/// into a panic on the node-to-fact lookup below it, so this arm pins the
/// invocation, not the validator.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn a_wiring_node_with_no_admitted_component_fact_refuses_the_mint() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");
    let (mut admin, task) = connect(&url).await;
    provision(&admin).await;
    seed_release(&admin, CATALOG_VERSION, "applied").await;
    seed_component(&admin, "http-request", COMPONENT_A).await;

    // Stored at its own hash and at the requested gated catalog version, so the
    // wiring-shaped predicates the mint checks first all pass. The only fact
    // this document fails is that "unadmitted" has no component-library row.
    let ungated = wiring("ungated", 1, "unadmitted", WiringTerminal::Respond);
    let ungated_hash = ungated.wiring_hash();
    inject_stored_wiring(&admin, &ungated, ungated_hash.as_str()).await;

    let ungated_target = BTreeSet::from([ReleaseWiringTarget {
        wiring_id: "ungated".to_string(),
        wiring_version: 1,
    }]);
    let empty_attachments = BTreeMap::new();
    let empty_registrations = BTreeMap::new();
    let request = MintReleaseManifest {
        tenant_id: TENANT,
        catalog_id: CATALOG,
        catalog_version: CATALOG_VERSION,
        wirings: &ungated_target,
        attachments: &empty_attachments,
        registrations: &empty_registrations,
    };
    let transaction = admin
        .transaction()
        .await
        .expect("open the ungated-component transaction");
    let refusal = mint_release_manifest(&transaction, &request)
        .await
        .expect_err("a wiring node with no admitted component fact refuses the mint");
    assert_eq!(refusal.kind(), MintManifestErrorKind::Component);
    transaction
        .rollback()
        .await
        .expect("close the ungated-component refusal");

    drop(admin);
    let _ = task.await;
}

/// Write one hand-authored interim document and return its path.
fn write_document<T: serde::Serialize>(directory: &Path, name: &str, document: &T) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(
        &path,
        serde_json::to_vec(document).expect("interim document serializes"),
    )
    .expect("write the interim release document");
    path
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL plus a \
            disposable repository in WAMN_RELEASE_MANIFEST_ARTIFACT_BASE and its \
            WAMN_REGISTRY_AUTH_FILE credential"]
async fn the_publish_verbs_carry_a_first_release_from_mint_to_oci() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");
    let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
        .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository");
    let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
        .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");

    let (admin, task) = connect(&url).await;
    let documents =
        std::env::temp_dir().join(format!("wamn-publish-release-{}", std::process::id()));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    // The wirings this release closes over are authored by the CLI verb from
    // their documents. Nothing between an empty environment and a first release
    // is hand SQL any more (wamn-1xb5).
    seed_publishable_release(&admin, &url, &documents).await;
    // The verb's precondition, satisfied: this project database is provisioned
    // for exactly the environment the catalog row labels the release with.
    provision_environment_policy(&admin, Some(ENVIRONMENT)).await;

    // The first release in this environment is minted by the CLI verb alone:
    // no hand SQL, no Rust test calling the mint, no source release to promote.
    let control_url = provision_control_plane(&url).await;
    publish_first_release(&url, &control_url, &documents)
        .await
        .expect("the interim publish verb mints a first release");

    let frozen = admin
        .query_one(
            "SELECT manifest_digest, canonical_bytes \
             FROM catalog.release_manifest_v2_snapshots \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("the CLI mint froze one v2 snapshot");
    let frozen_digest: String = frozen.get(0);
    let frozen_bytes: Vec<u8> = frozen.get(1);
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&frozen_bytes)
        .expect("the frozen snapshot is a canonical v2 manifest");
    assert_eq!(digest.as_str(), frozen_digest);
    assert_eq!(manifest.release.environment, ENVIRONMENT);
    assert_eq!(manifest.attachments, attachments());
    assert_eq!(manifest.registrations, registrations());

    // The bridge: the push verb reads those exact bytes out of the snapshot row
    // rather than being handed a file a human copied out of PostgreSQL. It runs
    // on the least-privileged production connection, whose forced row-level
    // security reveals the snapshot only under the claimed tenant.
    admin
        .batch_execute("ALTER ROLE wamn_app LOGIN PASSWORD 'release-reader'")
        .await
        .expect("give the app role a live-test password");
    let mut reader_url = url::Url::parse(&url).expect("the live database URL parses");
    reader_url
        .set_username("wamn_app")
        .expect("name the app role");
    reader_url
        .set_password(Some("release-reader"))
        .expect("carry the app role password");

    wamn_ctl::push_release_manifest::run(PushReleaseManifestArgs {
        manifest: None,
        database_url: Some(reader_url.to_string()),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        tenant: Some(TENANT.to_string()),
        catalog_id: Some(CATALOG.to_string()),
        catalog_version: Some(CATALOG_VERSION as u32),
        artifact_base: artifact_base.clone(),
        registry_auth_file: PathBuf::from(&registry_auth_file),
        insecure_registry: true,
        control_database_url: control_url.clone(),
    })
    .await
    .expect("the snapshot bridge publishes the minted release");

    // AlreadyPresent is returned only after the tag named by the frozen digest
    // is pulled back and its layout, config, and body proved byte-exact.
    let published = publish_release_manifest(
        &frozen_bytes,
        &artifact_base,
        true,
        Path::new(&registry_auth_file),
    )
    .await
    .expect("the published artifact reads back");
    assert_eq!(published.digest.as_str(), frozen_digest);
    assert_eq!(
        published.disposition,
        ReleaseManifestPublishDisposition::AlreadyPresent
    );

    std::fs::remove_dir_all(&documents).expect("remove the interim document directory");
    drop(admin);
    let _ = task.await;
}

/// The verify arm, live: the environment a release carries is checked against the
/// environment the connected project database was PROVISIONED for, and a release
/// that names another one refuses instead of keying an attestation to it.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn a_release_labelled_for_another_environment_than_this_database_refuses() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");

    let (admin, task) = connect(&url).await;
    let documents = std::env::temp_dir().join(format!(
        "wamn-publish-release-mismatch-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    seed_publishable_release(&admin, &url, &documents).await;
    // `catalog.catalogs.environment` labels this release `prod` (D18 puts no
    // constraint on that column at all); `reconcile-run-plane` projected this
    // database as `staging`. The label must lose.
    provision_environment_policy(&admin, Some("staging")).await;

    let control_url = provision_control_plane(&url).await;
    let refusal = publish_first_release(&url, &control_url, &documents)
        .await
        .expect_err("a release keyed to an environment this database is not");
    let typed = refusal
        .downcast_ref::<MintManifestError>()
        .unwrap_or_else(|| panic!("the publish verb refused untyped: {refusal:#}"));
    assert_eq!(
        typed.kind(),
        MintManifestErrorKind::EnvironmentPolicyMismatch,
        "refused as {refusal:#}"
    );
    let rendered = format!("{typed}");
    assert!(
        rendered.contains("environment-policy-environment-mismatch")
            && rendered.contains("\"prod\"")
            && rendered.contains("\"staging\""),
        "the live refusal does not name the two environments: {rendered}"
    );
    // Fail-closed: the refusal runs inside the mint's own transaction, so the
    // release it would have keyed is not frozen at all.
    assert_no_release_was_frozen(&admin).await;

    std::fs::remove_dir_all(&documents).expect("remove the interim document directory");
    drop(admin);
    let _ = task.await;
}

/// The absent arm, live (owner ruling 2026-08-26): a project database with no
/// projected environment policy for this tenant refuses on its OWN literal, and
/// the refusal names the verb that converges the policy.
///
/// This test also observes the grant and the row-security floor on a role that is
/// NOT the superuser the rest of this fixture connects as, because a superuser
/// fixture cannot prove either.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn a_release_with_no_projected_environment_policy_refuses() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");

    let (admin, task) = connect(&url).await;
    let documents = std::env::temp_dir().join(format!(
        "wamn-publish-release-absent-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    seed_publishable_release(&admin, &url, &documents).await;
    // The relation exists and is empty: nothing was ever projected for this
    // tenant, which is exactly the state a never-reconciled project is in.
    provision_environment_policy(&admin, None).await;

    let control_url = provision_control_plane(&url).await;
    let refusal = publish_first_release(&url, &control_url, &documents)
        .await
        .expect_err("a release published against an unprojected environment policy");
    let typed = refusal
        .downcast_ref::<MintManifestError>()
        .unwrap_or_else(|| panic!("the publish verb refused untyped: {refusal:#}"));
    assert_eq!(
        typed.kind(),
        MintManifestErrorKind::EnvironmentPolicyAbsent,
        "refused as {refusal:#}"
    );
    let rendered = format!("{typed}");
    assert!(
        rendered.contains("environment-policy-not-converged"),
        "the live absent refusal does not carry its own literal: {rendered}"
    );
    assert!(
        rendered.contains("reconcile-run-plane"),
        "the live absent refusal does not name its remedy verb: {rendered}"
    );
    assert_no_release_was_frozen(&admin).await;

    // The privilege question this precondition rests on, answered on a role that
    // does not bypass row security: `wamn_app` may SELECT the relation, and sees
    // only the tenant its `app.tenant` claim names. The superuser the mint
    // connects as in this fixture could not have shown either.
    admin
        .execute(
            "INSERT INTO wamn_run.environment_policies \
                   (tenant_id, expected_environment, durability_class) \
             VALUES ($1, $2, 'standard'), ('other-tenant', 'prod', 'standard')",
            &[&TENANT, &ENVIRONMENT],
        )
        .await
        .expect("project two tenants' environment facts");
    admin
        .batch_execute("ALTER ROLE wamn_app LOGIN PASSWORD 'w71b-policy-reader'")
        .await
        .expect("give the app role a live-test password");
    let mut reader_url = url::Url::parse(&url).expect("the live database URL parses");
    reader_url
        .set_username("wamn_app")
        .expect("name the app role");
    reader_url
        .set_password(Some("w71b-policy-reader"))
        .expect("carry the app role password");
    let (reader, reader_task) = connect(reader_url.as_str()).await;
    let bypasses: bool = reader
        .query_one(
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await
        .expect("observe the reading role")
        .get(0);
    assert!(!bypasses, "the reading role bypasses row security");
    let granted: bool = reader
        .query_one(
            "SELECT pg_catalog.has_table_privilege( \
                      current_user, 'wamn_run.environment_policies', 'SELECT')",
            &[],
        )
        .await
        .expect("observe the read grant")
        .get(0);
    assert!(
        granted,
        "the production app role cannot SELECT the provisioned environment fact"
    );
    for (claim, visible) in [(TENANT, 1_i64), ("other-tenant", 1), ("absent-tenant", 0)] {
        reader
            .execute("SELECT set_config('app.tenant', $1, false)", &[&claim])
            .await
            .expect("claim the reading tenant");
        let rows: i64 = reader
            .query_one(
                "SELECT count(*) FROM wamn_run.environment_policies WHERE tenant_id = $1",
                &[&claim],
            )
            .await
            .expect("read the provisioned environment fact")
            .get(0);
        assert_eq!(rows, visible, "row security let {claim:?} see {rows} rows");
    }
    reader
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("claim the release tenant");
    let crossed: i64 = reader
        .query_one(
            "SELECT count(*) FROM wamn_run.environment_policies WHERE tenant_id <> $1",
            &[&TENANT],
        )
        .await
        .expect("read across the tenant boundary")
        .get(0);
    assert_eq!(
        crossed, 0,
        "the claimed tenant read another tenant's policy"
    );

    std::fs::remove_dir_all(&documents).expect("remove the interim document directory");
    drop(reader);
    let _ = reader_task.await;
    drop(admin);
    let _ = task.await;
}

/// The cross-plane proof (wamn-0h0g.8.27, owner ruling 2026-08-27).
///
/// `catalog.deployment_attestations` lives ONLY on the control plane and its
/// foreign key targets the CONTROL copy of `catalog.releases`, while the publish
/// pipeline mints into the PROJECT copy — two separate relations that share a
/// name because database residency distinguishes the planes. This gate runs a
/// real project database and a real control database, both installed from the
/// production artifacts, and asserts the POST-STATE on the control server after
/// each step rather than the absence of an error.
///
/// The division of labour it pins is the ruling's: the MINT projects identity and
/// attests NOTHING, so a minted-but-unpushed release is reachable by the key and
/// still carries no attestation — which is exactly what `wamn-0h0g.13.54` means
/// by a candidate. Only the deployment writes the attestation.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn the_mint_projects_release_identity_and_leaves_it_unattested() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");

    let (admin, task) = connect(&url).await;
    let documents = std::env::temp_dir().join(format!(
        "wamn-publish-release-attestation-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    seed_publishable_release(&admin, &url, &documents).await;
    provision_environment_policy(&admin, Some(ENVIRONMENT)).await;
    let control_url = provision_control_plane(&url).await;
    let (control, control_task) = connect(&control_url).await;

    assert_eq!(
        control_state(&control).await,
        (0, 0, 0),
        "the fresh control store already holds this release"
    );

    publish_first_release(&url, &control_url, &documents)
        .await
        .expect("the interim publish verb mints a first release");

    // THE CANDIDATE POST-STATE. Identity arrived; nothing attested it.
    assert_eq!(
        control_state(&control).await,
        (1, 1, 0),
        "the mint did not project exactly one release identity and no attestation"
    );
    let projected = control
        .query_one(
            "SELECT environment, schema_version FROM catalog.catalogs \
              WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the projected catalog header");
    assert_eq!(projected.get::<_, String>(0), ENVIRONMENT);
    let projected_schema_version: String = projected.get(1);
    let minted_schema_version: String = admin
        .query_one(
            "SELECT schema_version FROM catalog.catalogs \
              WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the project catalog header")
        .get(0);
    assert_eq!(
        projected_schema_version, minted_schema_version,
        "the projection invented a catalog-model version the project plane never held"
    );

    let frozen_bytes: Vec<u8> = admin
        .query_one(
            "SELECT canonical_bytes FROM catalog.release_manifest_v2_snapshots \
              WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("the CLI mint froze one v2 snapshot")
        .get(0);
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&frozen_bytes)
        .expect("the frozen snapshot is a canonical v2 manifest");
    let coordinate = DeploymentCoordinate::new(ORG, PROJECT, &manifest.release);

    // A PUSH THAT NEVER REACHED A REGISTRY ATTESTS NOTHING. The attestation is
    // written strictly after the OCI push succeeds, so an unreachable registry
    // must leave the control plane exactly as the mint left it. `127.0.0.1:1` is
    // a port nothing can be listening on.
    let auth_file = unreachable_registry_credential(&documents);
    let unpublished = wamn_ctl::push_release_manifest::run(PushReleaseManifestArgs {
        manifest: None,
        database_url: Some(url.clone()),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        tenant: Some(TENANT.to_string()),
        catalog_id: Some(CATALOG.to_string()),
        catalog_version: Some(CATALOG_VERSION as u32),
        artifact_base: format!("{UNREACHABLE_REGISTRY}/wamn-live/release-manifest"),
        registry_auth_file: auth_file.clone(),
        insecure_registry: true,
        control_database_url: control_url.clone(),
    })
    .await
    .expect_err("a push that cannot reach its registry");
    assert!(
        unpublished.downcast_ref::<AttestationError>().is_none(),
        "the push reached the attestation write before publishing any bytes: {unpublished:#}"
    );
    assert_eq!(
        control_state(&control).await,
        (1, 1, 0),
        "a push that published nothing still attested a deployment"
    );

    // THE DEPLOYMENT EVENT. One attestation, and the foreign key resolves it
    // against the identity the mint projected.
    attest_deployment(&control_url, &coordinate, &digest)
        .await
        .expect("the deployed release is attested on the control plane");
    assert_eq!(
        control_state(&control).await,
        (1, 1, 1),
        "the deployment did not record exactly one attestation"
    );
    let (attested_hash, attested_at) = attested_content(&control)
        .await
        .expect("the attestation is recorded");
    assert_eq!(attested_hash, digest.as_str());

    // AN EXACT RETRY CONVERGES rather than raising the routine's content
    // conflict on a second reading of the clock: `attested_at` is attested
    // CONTENT, so the retry must present the instant already recorded.
    attest_deployment(&control_url, &coordinate, &digest)
        .await
        .expect("an exact re-attestation converges");
    assert_eq!(
        attested_content(&control).await,
        Some((attested_hash.clone(), attested_at.clone())),
        "the retry rewrote or duplicated the attested fact"
    );

    // DIFFERENT BYTES AT THE SAME COORDINATE REFUSE, and the recorded fact stands.
    let other_digest = ManifestDigest::parse(
        "sha256:9999999999999999999999999999999999999999999999999999999999999999",
    )
    .expect("fixture digest is canonical");
    let conflict = attest_deployment(&control_url, &coordinate, &other_digest)
        .await
        .expect_err("re-attesting a coordinate with other bytes");
    let typed_conflict = conflict
        .downcast_ref::<AttestationError>()
        .unwrap_or_else(|| panic!("the conflict was untyped: {conflict:#}"));
    assert_eq!(
        typed_conflict.kind(),
        AttestationErrorKind::ContentConflict,
        "the routine's own refusal was classified as {:?}: {typed_conflict}",
        typed_conflict.kind()
    );
    assert_eq!(
        attested_content(&control).await,
        Some((attested_hash, attested_at)),
        "the refused re-attestation still moved the recorded fact"
    );

    // A COORDINATE THE MINT NEVER PROJECTED CANNOT BE ATTESTED INTO EXISTENCE.
    // This is the referential guarantee `wamn-0h0g.13.54` rests on, observed as
    // the server's own foreign-key refusal.
    let unminted = DeploymentCoordinate {
        catalog_version: coordinate.catalog_version + 1,
        ..coordinate.clone()
    };
    let refusal = attest_deployment(&control_url, &unminted, &digest)
        .await
        .expect_err("attesting a release identity that was never projected");
    let typed = refusal
        .downcast_ref::<AttestationError>()
        .unwrap_or_else(|| panic!("the foreign-key refusal was untyped: {refusal:#}"));
    assert_eq!(typed.kind(), AttestationErrorKind::Storage);
    assert!(
        typed.driver().contains("foreign key"),
        "the refusal did not come from the cross-plane key: {typed}"
    );
    let unminted_rows: i64 = control
        .query_one(
            "SELECT count(*) FROM catalog.deployment_attestations \
              WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&TENANT, &CATALOG, &(CATALOG_VERSION + 1)],
        )
        .await
        .expect("count the unminted coordinate's attestations")
        .get(0);
    assert_eq!(unminted_rows, 0);

    // THE PROJECTION IS INSERT-OR-VERIFY-IDENTICAL, not a silent no-op: an exact
    // re-projection converges, and one carrying other catalog facts refuses.
    project_release_identity(&control_url, &coordinate, &minted_schema_version)
        .await
        .expect("an exact re-projection converges");
    assert_eq!(control_state(&control).await, (1, 1, 1));
    let drifted = project_release_identity(&control_url, &coordinate, "9999.0.0")
        .await
        .expect_err("re-projecting the coordinate under another catalog-model version");
    assert_eq!(
        drifted
            .downcast_ref::<AttestationError>()
            .unwrap_or_else(|| panic!("the projection conflict was untyped: {drifted:#}"))
            .kind(),
        AttestationErrorKind::IdentityProjectionConflict
    );
    let still: String = control
        .query_one(
            "SELECT schema_version FROM catalog.catalogs \
              WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3",
            &[&TENANT, &CATALOG, &CATALOG_VERSION],
        )
        .await
        .expect("read the projected catalog header back")
        .get(0);
    assert_eq!(
        still, minted_schema_version,
        "the refused re-projection still rewrote the projected fact"
    );

    // THE AUTHORITY FLOOR, asked of the server about a role that is neither a
    // superuser nor a row-security bypasser — the only kind of role this
    // question means anything for. The control author is the one non-owner
    // principal this store grants anything to, and it reaches none of the
    // cross-plane write path.
    let bypasses: bool = control
        .query_one(
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = 'wamn_control_author'",
            &[],
        )
        .await
        .expect("observe the control author role")
        .get(0);
    assert!(!bypasses, "the control author bypasses row security");
    for (relation, privilege) in [
        ("catalog.deployment_attestations", "SELECT"),
        ("catalog.deployment_attestations", "INSERT"),
        ("catalog.releases", "INSERT"),
        ("catalog.catalogs", "SELECT"),
        ("catalog.catalogs", "INSERT"),
    ] {
        let held: bool = control
            .query_one(
                "SELECT pg_catalog.has_table_privilege('wamn_control_author', $1, $2)",
                &[&relation, &privilege],
            )
            .await
            .expect("observe the control author's table privilege")
            .get(0);
        assert!(!held, "the control author holds {privilege} on {relation}");
    }
    for routine in [
        "catalog.register_deployment_attestation(text,text,int,text,text,text,text,timestamptz)",
        "catalog.project_release_identity(text,text,int,text,text)",
    ] {
        let held: bool = control
            .query_one(
                "SELECT pg_catalog.has_function_privilege('wamn_control_author', $1, 'EXECUTE')",
                &[&routine],
            )
            .await
            .expect("observe the control author's routine privilege")
            .get(0);
        assert!(!held, "the control author may execute {routine}");
    }

    std::fs::remove_dir_all(&documents).expect("remove the interim document directory");
    drop(control);
    let _ = control_task.await;
    drop(admin);
    let _ = task.await;
}

/// A minimal catalog document, so `promote` can read the source release's stored
/// model and `migrate-catalog` has a target to apply.
fn seed_catalog_document() -> serde_json::Value {
    json!({
        "schema-version": "0.1",
        "catalog-id": CATALOG,
        "version": CATALOG_VERSION,
        "entities": [{
            "id": "orders",
            "name": "orders",
            "fields": [{"id": "code", "name": "code", "type": {"kind": "text"}}],
        }],
    })
}

/// Whether `wamn_app` still holds the membership `ensure_wamn_app_role` revokes.
///
/// Role state is CLUSTER-global, so a refusal raised after that revoke could not
/// take it back. This is the one witness a later failure cannot fake.
async fn scenario_author_membership(client: &Client) -> bool {
    client
        .query_one(
            "SELECT EXISTS ( \
               SELECT FROM pg_catalog.pg_auth_members member \
               JOIN pg_catalog.pg_roles granted ON granted.oid = member.roleid \
               JOIN pg_catalog.pg_roles holder ON holder.oid = member.member \
               WHERE granted.rolname = 'wamn_scenario_author' \
                 AND holder.rolname = 'wamn_app')",
            &[],
        )
        .await
        .expect("read the scenario-author membership")
        .get(0)
}

async fn plant_role_witness(client: &Client) {
    client
        .batch_execute(
            "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
             GRANT wamn_scenario_author TO wamn_app",
        )
        .await
        .expect("plant the cluster-global witness the wrong-plane path would revoke");
}

/// Live proof for wamn-0h0g.12.183: the two verbs that bootstrapped the
/// cluster-global role BEFORE the plane refusal no longer do.
///
/// `wamn-0h0g.12.180` put the control-plane refusal at the top of
/// `ensure_catalog_storage`, ahead of `ensure_wamn_app_role`, precisely because
/// role state is CLUSTER-wide and a refusal that fires after it has already
/// mutated the cluster is not fail-closed. `promote` and `migrate-catalog` each
/// called the role bootstrap themselves one line earlier and defeated it.
///
/// Both verbs run here as production runs them — `promote` against a real minted
/// source release, `migrate-catalog` against a real catalog document — with a
/// real control database as the target. The assertion is the POST-STATE of
/// cluster-global role state, not an exit status: a planted membership must
/// SURVIVE, because the pre-change verbs revoked it before refusing.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_RELEASE_MANIFEST_MINT_PG_URL"]
async fn promote_and_migrate_catalog_refuse_a_control_database_before_minting_any_role() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");

    let (admin, task) = connect(&url).await;
    let documents =
        std::env::temp_dir().join(format!("wamn-control-plane-refusal-{}", std::process::id()));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    seed_publishable_release(&admin, &url, &documents).await;
    provision_environment_policy(&admin, Some(ENVIRONMENT)).await;
    let document = seed_catalog_document();
    admin
        .execute(
            "UPDATE catalog.catalogs SET document = $4::text::jsonb \
              WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3",
            &[
                &TENANT,
                &CATALOG,
                &CATALOG_VERSION,
                &serde_json::to_string(&document).expect("the seed catalog serializes"),
            ],
        )
        .await
        .expect("store the source catalog document promote reads");
    let control_url = provision_control_plane(&url).await;
    publish_first_release(&url, &control_url, &documents)
        .await
        .expect("the source release promote reads is minted by the CLI verb");
    let (control, control_task) = connect(&control_url).await;
    let control_inventory = tables(&control, "catalog").await;
    assert!(
        control_inventory.contains(&"authoring_command_audit".to_string()),
        "the control store does not carry the witness the refusal reads: {control_inventory:?}"
    );

    let credential = unreachable_registry_credential(&documents);
    let target_document = documents.join("target.json");
    std::fs::write(
        &target_document,
        serde_json::to_vec(&document).expect("the target catalog serializes"),
    )
    .expect("write the migrate-catalog target");

    // ARM 1 — `promote`, driven from a real source release into a control
    // database as its target.
    plant_role_witness(&admin).await;
    let refusal = wamn_ctl::promote::run(wamn_ctl::promote::PromoteArgs {
        source_database_url: url.clone(),
        target_database_url: control_url.clone(),
        tenant: TENANT.to_string(),
        catalog_id: CATALOG.to_string(),
        catalog_version: CATALOG_VERSION as u32,
        source_environment: ENVIRONMENT.to_string(),
        target_environment: "canary".to_string(),
        schema: "public".to_string(),
        run_schema: RUN_SCHEMA.to_string(),
        artifact_base: format!("{UNREACHABLE_REGISTRY}/wamn-live/components"),
        registry_auth_file: credential.clone(),
        insecure_registry: true,
        principal: "live-gate".to_string(),
        reason: "promote-release".to_string(),
    })
    .await
    .expect_err("promote must refuse a control database as its target");
    assert!(
        format!("{refusal:#}").contains(CATALOG_PLANE_RESIDENCY_REFUSAL),
        "promote rejected the control plane for the wrong reason: {refusal:#}"
    );
    assert!(
        scenario_author_membership(&admin).await,
        "promote's refusal ran AFTER ensure_wamn_app_role: cluster-global role \
         state was already mutated on a database the verb then rejected"
    );

    // ARM 2 — `migrate-catalog`, same target, same witness.
    plant_role_witness(&admin).await;
    let refusal = wamn_ctl::migrate_catalog::run(wamn_ctl::migrate_catalog::MigrateCatalogArgs {
        admin_database_url: control_url.clone(),
        tenant: TENANT.to_string(),
        environment: ENVIRONMENT.to_string(),
        schema: "public".to_string(),
        target: target_document.clone(),
        base: None,
        dry_run: false,
        skip_reconcile_replica_identity: true,
    })
    .await
    .expect_err("migrate-catalog must refuse a control database");
    assert!(
        format!("{refusal:#}").contains(CATALOG_PLANE_RESIDENCY_REFUSAL),
        "migrate-catalog rejected the control plane for the wrong reason: {refusal:#}"
    );
    assert!(
        scenario_author_membership(&admin).await,
        "migrate-catalog's refusal ran AFTER ensure_wamn_app_role: cluster-global \
         role state was already mutated on a database the verb then rejected"
    );

    // Neither verb may have installed project storage into the control plane.
    assert_eq!(
        tables(&control, "catalog").await,
        control_inventory,
        "project catalog storage reached the control plane"
    );

    admin
        .batch_execute("REVOKE wamn_scenario_author FROM wamn_app")
        .await
        .expect("hand the cluster's role state back");
    std::fs::remove_dir_all(&documents).expect("remove the interim document directory");
    drop(control);
    let _ = control_task.await;
    drop(admin);
    let _ = task.await;
}
