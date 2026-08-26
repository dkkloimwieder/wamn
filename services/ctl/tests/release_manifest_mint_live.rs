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
    AttachmentKind, CatalogIdentityError, DefinitionHash, SERVING_MANIFEST_FORMAT_VERSION,
    ServingAttachment, ServingManifest, ServingRegistration, ServingRegistrationInput,
    ServingRelease, ServingWiring, WiringDocument, WiringEventOperation, WiringNode,
    WiringTerminal,
};
use wamn_ctl::author_wiring::{
    AuthorWiringArgs, AuthorWiringErrorKind, AuthorWiringRequest, author_wiring,
};
use wamn_ctl::publish_catalog::ensure_catalog_storage;
use wamn_ctl::publish_release::{
    MintManifestErrorKind, MintReleaseManifest, PublishReleaseArgs, RELEASE_MANIFEST_MINT_REFUSAL,
    ReleaseWiringTarget, mint_release_manifest,
};
use wamn_ctl::push_release_manifest::{
    PushReleaseManifestArgs, ReleaseManifestPublishDisposition, publish_release_manifest,
};
use wamn_execution_contract::EntryKind;
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

#[test]
fn the_production_mint_has_no_legacy_conversion_arm() {
    const SOURCE: &str = include_str!("../src/publish_release.rs");
    for retired in [
        "catalog.release_flows",
        "catalog.execution_bundles",
        "ExecutionPlan",
        "ServingFlow",
        "candidate_draft",
        "reachable_flows",
    ] {
        assert!(
            !SOURCE.contains(retired),
            "v2 mint contains legacy conversion token {retired:?}"
        );
    }
    assert_eq!(SOURCE.matches("validate_wiring_compatibility(").count(), 1);
    for current in [
        "FROM catalog.component_library",
        "FROM catalog.wirings",
        "INSERT INTO catalog.release_components",
        "INSERT INTO catalog.release_manifest_v2_snapshots",
        "document.wiring_hash()",
    ] {
        assert!(SOURCE.contains(current), "v2 mint omits {current:?}");
    }
    assert_eq!(
        RELEASE_MANIFEST_MINT_REFUSAL,
        "release-manifest-mint-refused"
    );
    assert_eq!(
        MintManifestErrorKind::ClosureConflict.as_str(),
        "closure-conflict"
    );
}

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

    let documents =
        std::env::temp_dir().join(format!("wamn-publish-release-{}", std::process::id()));
    std::fs::create_dir_all(&documents).expect("create the interim document directory");
    let attachments_path = write_document(&documents, "attachments.json", &attachments());
    let registrations_path = write_document(&documents, "registrations.json", &registrations());

    // The wirings this release closes over are authored by the CLI verb from
    // their documents. Nothing between an empty environment and a first release
    // is hand SQL any more (wamn-1xb5).
    for document in [&orders, &shipping] {
        record_green_report(&admin, document).await;
        wamn_ctl::author_wiring::run(AuthorWiringArgs {
            database_url: url.clone(),
            // Co-resident by fixture only; see `provision_gate_reports`.
            control_database_url: url.clone(),
            tenant: TENANT.to_string(),
            catalog_id: CATALOG.to_string(),
            gated_catalog_version: CATALOG_VERSION as u32,
            wiring_document: write_document(
                &documents,
                &format!("{}.json", document.wiring_id),
                document,
            ),
        })
        .await
        .expect("the authorship verb gates and stores one wiring document");
    }

    // The first release in this environment is minted by the CLI verb alone:
    // no hand SQL, no Rust test calling the mint, no source release to promote.
    wamn_ctl::publish_release::run(PublishReleaseArgs {
        database_url: url.clone(),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        tenant: TENANT.to_string(),
        catalog_id: CATALOG.to_string(),
        catalog_version: CATALOG_VERSION as u32,
        wirings: targets().into_iter().collect(),
        attachments: attachments_path,
        registrations: registrations_path,
    })
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
