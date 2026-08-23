//! Live proof that serving-manifest v2 is minted from current catalog facts.
//!
//! Set `WAMN_RELEASE_MANIFEST_MINT_PG_URL` to a disposable PostgreSQL 18
//! database. The test drops and recreates its `catalog` schema.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AttachmentKind, CatalogIdentityError, DefinitionHash, SERVING_MANIFEST_FORMAT_VERSION,
    ServingAttachment, ServingManifest, ServingRegistration, ServingRegistrationInput,
    ServingRelease, ServingWiring, WiringDocument, WiringEventOperation, WiringNode,
    WiringTerminal,
};
use wamn_ctl::publish_catalog::ensure_catalog_storage;
use wamn_ctl::publish_release::{
    MintManifestErrorKind, MintReleaseManifest, RELEASE_MANIFEST_MINT_REFUSAL, ReleaseWiringTarget,
    mint_release_manifest,
};
use wamn_flow::EntryKind;
use wamn_schema_control::{
    Attachment as ExposureAttachment, AttachmentKind as ExposureAttachmentKind, ExposureRelease,
    FlowExposure, HttpRoute, Source, SourceKind, resolve_exposure,
};

const TENANT: &str = "manifest-mint-tenant";
const CATALOG: &str = "manifest-mint-catalog";
const CATALOG_VERSION: i32 = 3;
const ENVIRONMENT: &str = "prod";
const COMPONENT_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const COMPONENT_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const WRONG_GRAPH: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const DEFINITION: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
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
            graph_hash: WRONG_GRAPH.to_string(),
        }]),
        attachments: BTreeMap::from([(
            "orders-http".to_string(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                wiring_id: "orders".to_string(),
                wiring_version: 1,
                definition_hash: resolved.definition_hash.clone(),
                definition: json!({"id": "orders-http", "kind": "http"}),
                auth_policy: json!({"mode": "none"}),
            },
        )]),
        registrations: BTreeMap::new(),
    };
    let (admitted, _) = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())
        .expect("the serving-manifest boundary admits the minted definition hash");
    assert_eq!(
        admitted.attachments["orders-http"].definition_hash,
        resolved.definition_hash
    );

    let mut bare_hash = manifest;
    bare_hash
        .attachments
        .get_mut("orders-http")
        .expect("fixture attachment")
        .definition_hash = admitted_hash
        .as_str()
        .strip_prefix("sha256:")
        .expect("admitted digest prefix")
        .to_string();
    assert_eq!(
        ServingManifest::from_canonical_bytes(&bare_hash.canonical_bytes()),
        Err(CatalogIdentityError::InvalidDigest {
            field: "definition-hash"
        })
    );
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
        "REFERENCES catalog.release_manifests",
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
            "INSERT INTO catalog.release_manifests \
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
                    operation, component_digest, imports, imports_fingerprint, input_ports, \
                    output_ports, parameters) \
             VALUES ($1, $2, $3, $4, '0.1', 'call', $5, \
                     '[]'::jsonb, $6, '[]'::jsonb, '[]'::jsonb, '[]'::jsonb)",
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

async fn seed_wiring(admin: &Client, document: &WiringDocument, graph_hash: &str) {
    let version = i32::try_from(document.version).expect("fixture version fits storage");
    let graph = serde_json::to_string(document).expect("wiring serializes");
    admin
        .execute(
            "INSERT INTO catalog.wirings \
                   (tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
                    graph_json, wiring_hash, gate_report_id) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, 'gate-report')",
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
        .expect("seed a gated wiring");
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
            definition_hash: DEFINITION.to_string(),
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
    let url = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL")
        .expect("WAMN_RELEASE_MANIFEST_MINT_PG_URL names a disposable PostgreSQL 18 database");
    let (mut admin, task) = connect(&url).await;
    provision(&admin).await;
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
    let orders_hash = orders.wiring_hash().to_string();
    let shipping_hash = shipping.wiring_hash().to_string();
    seed_wiring(&admin, &orders, &orders_hash).await;
    seed_wiring(&admin, &shipping, &shipping_hash).await;

    let corrupt = wiring(
        "corrupt",
        3,
        "transform",
        WiringTerminal::emit("orders", WiringEventOperation::Insert),
    );
    seed_wiring(&admin, &corrupt, WRONG_GRAPH).await;
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
    let _ = task.await;
}
