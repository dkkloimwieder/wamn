//! The release-manifest mint against a live control PostgreSQL (wamn-0h0g.15.14).
//!
//! `services/ctl/src/publish_release.rs`'s in-module live test proves the mint
//! step A performs on the release step A itself builds: one member, no exposure,
//! no candidate. This proves the parts that release cannot reach — the call-edge
//! adjacency between two members, the attachment projection and the source
//! document it resolves against, the candidate overlay where
//! `binding-base-artifact` and `source-artifact` deliberately diverge, and that a
//! repeated mint over identical content names the same bytes.
//!
//! # It runs as the store owner, not as a superuser
//!
//! The mint transaction takes `SET LOCAL ROLE wamn_system` before it mints. That
//! is the production principal and it is the only one under which the tenant
//! claim is load-bearing: every relation the mint reads carries FORCE ROW LEVEL
//! SECURITY, which applies to the owner and is bypassed by a superuser. Seeding
//! stays on the superuser connection, where the same bypass is what makes the
//! seed simple.
//!
//! # Hermetic at CLUSTER scope, not merely per-database
//!
//! Dropping and recreating this database's schemas is not enough. PostgreSQL
//! ROLES and DATABASE ACLs are cluster-wide, and the control bootstrap's
//! author-boundary guard reads one of those cluster-wide facts — see
//! [`establish_control_connect_posture`]. The preamble therefore constructs the
//! contaminated cluster on purpose and then converges production's posture over
//! it, so this gate behaves identically on a pristine container and on one that
//! already looks like a deployment.
//!
//! Set `WAMN_RELEASE_MANIFEST_MINT_PG_URL` to a throwaway PostgreSQL 18. The test
//! drops and recreates every control schema in that database, and takes PUBLIC's
//! `CONNECT` off that one database.

use std::collections::{BTreeMap, BTreeSet};

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    CALLABLE_CONTRACT_VERSION, CallableContract, CallableEffectCeiling, CallableReturnContract,
    ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanBody, ExecutionPlanEdge,
    ExecutionPlanNode, ExecutionPlanV2, ExecutionRuntimeRevision, ExecutionSourceMapEntry,
    RootTerminalBehavior, SERVING_MANIFEST_FORMAT_VERSION, ServingManifest, ServingRegistration,
    ServingRelease, entry_input_schema_hash, execution_bundle_hash,
};
use wamn_ctl::publish_release::{
    MintManifestErrorKind, MintReleaseManifest, MintedReleaseManifest, mint_release_manifest,
};

mod mint_vector {
    include!("../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs");
}

const TENANT: &str = "manifest-mint-tenant";
const CATALOG_ID: &str = "manifest-mint-catalog";
const CATALOG_VERSION: i32 = 3;
const ENVIRONMENT: &str = "prod";

const ROOT_FLOW: &str = "orders";
const ROOT_FLOW_VERSION: i32 = 1;
const ROOT_ARTIFACT: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

const CALLEE_FLOW: &str = "shipping";
const CALLEE_FLOW_VERSION: i32 = 2;
const CALLEE_ARTIFACT: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The candidate's own artifact. A draft artifact is never written to
/// `catalog.flow_artifacts`, so it can never own a connection-binding row — which
/// is exactly why the released base has to travel beside it.
const DRAFT_ARTIFACT: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const CANDIDATE_FLOW_VERSION: i32 = 3;
const VALIDATED_DRAFT_HASH: &str =
    "sha256:4444444444444444444444444444444444444444444444444444444444444444";

const DEFINITION_HASH: &str =
    "sha256:5555555555555555555555555555555555555555555555555555555555555555";
const SOURCE_HASH: &str = "sha256:6666666666666666666666666666666666666666666666666666666666666666";

// ---------------------------------------------------------------------------
// Plan fixtures.
// ---------------------------------------------------------------------------

fn node(local: &str, node_type: &str, config: serde_json::Value) -> ExecutionPlanNode {
    ExecutionPlanNode {
        local_node_id: ExecutionNodeId::new(local).expect("a slug node id"),
        source_node_id: local.to_string(),
        // A call-flow node inherits its callee's authority, so the plan model
        // refuses to admit one that claims to be pure.
        effect_policy: if node_type == "call-flow" {
            ExecutionEffectPolicy::Effectful
        } else {
            ExecutionEffectPolicy::Pure
        },
        node_type: node_type.to_string(),
        config,
        source_connection_requirement: None,
    }
}

fn edge(source: &str, destination: &str) -> ExecutionPlanEdge {
    ExecutionPlanEdge {
        source: ExecutionNodeId::new(source).expect("a slug node id"),
        source_port: "out".to_string(),
        destination: ExecutionNodeId::new(destination).expect("a slug node id"),
        destination_port: None,
        fan_out_ordinal: 0,
    }
}

fn source_map(locals: &[&str]) -> Vec<ExecutionSourceMapEntry> {
    locals
        .iter()
        .map(|local| ExecutionSourceMapEntry {
            local_node_id: ExecutionNodeId::new(*local).expect("a slug node id"),
            source_node_id: (*local).to_string(),
        })
        .collect()
}

fn runtime_revision() -> ExecutionRuntimeRevision {
    ExecutionRuntimeRevision {
        flowrunner_component_digest: format!("sha256:{}", "a".repeat(64)),
        effect_provider_revision: format!("sha256:{}", "b".repeat(64)),
        host_effect_contract_version: wamn_catalog::HOST_EFFECT_CONTRACT_VERSION.to_string(),
    }
}

/// A valid own-flow plan; `callees` becomes one `call-flow` node each.
fn plan(root_artifact: &str, callees: &[&str]) -> ExecutionPlanV2 {
    let guard = serde_json::Value::Bool(true);
    let sites: Vec<String> = (0..callees.len())
        .map(|index| format!("call-{index}"))
        .collect();
    let mut nodes = vec![
        node(
            "request",
            "request",
            serde_json::json!({"input-schema": true}),
        ),
        node("respond", "respond", serde_json::json!({"status": 200})),
    ];
    let mut edges = vec![];
    let mut locals = vec!["request", "respond"];
    let mut previous = "request".to_string();
    for (site, callee) in sites.iter().zip(callees) {
        nodes.push(node(
            site,
            "call-flow",
            serde_json::json!({"site": site, "flow-id": callee}),
        ));
        edges.push(edge(&previous, site));
        previous = site.clone();
    }
    edges.push(edge(&previous, "respond"));
    locals.extend(sites.iter().map(String::as_str));
    locals.sort_unstable();

    let body = ExecutionPlanBody {
        entry_instruction: ExecutionNodeId::new("request").expect("a slug node id"),
        nodes,
        edges,
        root_terminal_behavior: RootTerminalBehavior::Respond {
            responders: vec![ExecutionNodeId::new("respond").expect("a slug node id")],
        },
        callable_contract: Some(CallableContract {
            version: CALLABLE_CONTRACT_VERSION.to_string(),
            input_schema_hash: entry_input_schema_hash(&guard),
            return_contract: CallableReturnContract::UntypedJsonBody,
            effect_ceiling: CallableEffectCeiling::Effectful,
        }),
        entry_input_schema_guard: guard,
        source_map: source_map(&locals),
    };
    ExecutionPlanV2::new(runtime_revision(), root_artifact, body)
        .expect("the fixture plan is valid")
}

fn plan_bytes(plan: &ExecutionPlanV2) -> (String, Vec<u8>) {
    let bytes = serde_json::to_vec(plan).expect("a plan serializes");
    (execution_bundle_hash(&bytes), bytes)
}

// ---------------------------------------------------------------------------
// Seeding.
// ---------------------------------------------------------------------------

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the control database");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

/// Construct the contaminated cluster, then converge production's posture on it.
///
/// `deploy/sql/control-portable-store.sql` refuses with
/// `control-author-authority-boundary-violated` when `wamn_scenario_author`
/// holds `CONNECT` on the current database — the control plane's author and the
/// project plane's author must never be reachable from one store. That guard is
/// CORRECT and must not be relaxed to make this gate pass.
///
/// The trap is that it reads a CLUSTER-WIDE fact from a per-database preamble.
/// Roles are cluster-global, and a stock database grants `CONNECT` to PUBLIC, so
/// the moment `wamn_scenario_author` exists ANYWHERE in the cluster the guard is
/// true for EVERY database in it. Any sibling gate that creates the role is
/// enough — `services/ctl/tests/replica_identity_unreadable_live.rs` does, on a
/// different database — and so is every real deployment, because
/// `deploy/sql/postgres-init.sql` creates it. So the gate could only pass on a
/// cluster that did not resemble production, and it failed by NAMING A GUARD,
/// which reads as a code defect rather than an environment one.
///
/// So this builds the failing state deliberately rather than hoping to avoid it:
/// create the role exactly as `postgres-init.sql` does, restore PUBLIC's stock
/// `CONNECT`, assert the guard's fact is TRUE, and only then converge the posture
/// production converges — PUBLIC holds no `CONNECT`, the floor
/// `wamn_control_provision::sql::revoke_public_connect_floor_sql` establishes and
/// the same shape `protected_relations_live` and `terminalize_effect_uncertain_live`
/// already use. The revoke is scoped to `current_database()` rather than reusing
/// that cluster-wide floor builder: this gate shares its cluster with its siblings
/// and must not reach across it.
///
/// PUBLIC's grant is the only route, so revoking it is the whole fix. A revoke
/// aimed at `wamn_scenario_author` itself was measured and dropped: nothing in
/// this repository grants that role database `CONNECT` directly, so no probe
/// could kill it.
///
/// It is ONE statement, and it has to stay one. Restoring PUBLIC's `CONNECT` and
/// taking it away again in separate statements would commit a window in which
/// this database does grant `CONNECT` to PUBLIC — and a sibling gate samples that
/// exact fact CLUSTER-WIDE
/// (`effect_writer_generation_live` asserts `sql::public_connect_databases_sql()`
/// comes back empty). Inside one implicit transaction the grant is never visible
/// to another session and only the revoke commits, so fixing one cross-gate
/// coupling does not open another. The assertions therefore run in SQL, where
/// they can see the uncommitted grant, rather than as Rust round trips that
/// cannot.
async fn establish_control_connect_posture(admin: &Client) {
    admin
        .batch_execute(
            "DO $posture$ BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles \
                              WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO PUBLIC', current_database()); \
               ASSERT (SELECT pg_catalog.has_database_privilege( \
                                oid, pg_catalog.current_database(), 'CONNECT') \
                         FROM pg_catalog.pg_roles \
                        WHERE rolname = 'wamn_scenario_author'), \
                 'the project author must be admitted onto this database before the \
                  posture is converged, or this preamble proves nothing'; \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', current_database()); \
               ASSERT NOT (SELECT pg_catalog.has_database_privilege( \
                                    oid, pg_catalog.current_database(), 'CONNECT') \
                             FROM pg_catalog.pg_roles \
                            WHERE rolname = 'wamn_scenario_author'), \
                 'the project author still reaches this control database, so the \
                  bootstrap author-boundary guard is about to refuse — the posture, \
                  not the guard, is what has to change'; \
             END $posture$;",
        )
        .await
        .expect("converge production's CONNECT posture on the control database");
}

async fn provision_control_store(admin: &Client) {
    establish_control_connect_posture(admin).await;
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ BEGIN IF NOT EXISTS \
               (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; END $$; \
             DO $$ BEGIN EXECUTE format( \
               'GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;",
        )
        .await
        .expect("reset the control schemas");
    admin
        .batch_execute(wamn_control_provision::sql::ensure_control_author_acl_role_sql())
        .await
        .expect("mint the stable control-author ACL role");
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system;\n{}\n{}\nRESET ROLE;",
            wamn_control_provision::SYSTEM_SCHEMA_SQL,
            wamn_control_provision::CONTROL_PORTABLE_STORE_SQL,
        ))
        .await
        .expect("apply the fresh control bootstrap");
}

async fn seed_artifact(admin: &Client, flow_id: &str, flow_version: i32, artifact: &str) {
    admin
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id, flow_id, flow_version, schema_version, graph_json, graph_hash, \
                artifact_hash) \
             VALUES ($1, $2, $3, '0.1', '{}'::jsonb, $4, $4)",
            &[&TENANT, &flow_id, &flow_version, &artifact],
        )
        .await
        .expect("seed an immutable source artifact");
}

async fn seed_bundle(admin: &Client, hash: &str, bytes: &[u8]) {
    let byte_length = i32::try_from(bytes.len()).expect("a fixture plan fits i32");
    admin
        .execute(
            "INSERT INTO catalog.execution_bundles \
               (tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length) \
             VALUES ($1, $2, '0.1', $3, $4)",
            &[&TENANT, &hash, &bytes, &byte_length],
        )
        .await
        .expect("seed an execution bundle");
}

async fn seed_member(admin: &Client, flow_id: &str, flow_version: i32, bundle_hash: &str) {
    admin
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id, catalog_id, catalog_version, flow_id, flow_version, \
                execution_bundle_hash) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &flow_id,
                &flow_version,
                &bundle_hash,
            ],
        )
        .await
        .expect("seed a release member");
}

async fn seed_release_base(admin: &Client) {
    admin
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the seeding session");
    admin
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ($1, $2, $3, $4, '0.1', 'applied')",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION, &ENVIRONMENT],
        )
        .await
        .expect("seed the catalog release");
    admin
        .execute(
            "INSERT INTO catalog.release_manifests (tenant_id, catalog_id, catalog_version) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await
        .expect("seed the release identity row");
}

async fn seed_exposure(admin: &Client) {
    admin
        .execute(
            "INSERT INTO catalog.release_exposure_manifests \
               (tenant_id, catalog_id, catalog_version, definitions_json) \
             VALUES ($1, $2, $3, '{}'::jsonb)",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await
        .expect("seed the exposure manifest");
    admin
        .execute(
            "INSERT INTO catalog.release_sources \
               (tenant_id, catalog_id, catalog_version, source_id, source_kind, \
                definition_json, source_hash) \
             VALUES ($1, $2, $3, 'public', 'auth', $4::text::jsonb, $5)",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &r#"{"mode":"none"}"#,
                &SOURCE_HASH,
            ],
        )
        .await
        .expect("seed the resolved source");
    admin
        .execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id, catalog_id, catalog_version, attachment_id, attachment_kind, \
                flow_id, source_id, definition_hash, definition_json, route_host, \
                route_path, route_template, route_method) \
             VALUES ($1, $2, $3, 'orders-http', 'http', $4, 'public', $5, $6::text::jsonb, \
                     '*', '/orders', '/orders', 'POST')",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &ROOT_FLOW,
                &DEFINITION_HASH,
                &r#"{"id":"orders-http","kind":"http","run-deadline-ms":30000}"#,
            ],
        )
        .await
        .expect("seed the resolved attachment");
}

async fn seed_candidate_draft(admin: &Client, bundle_hash: &str) {
    admin
        .execute(
            "INSERT INTO catalog.validated_flow_drafts \
               (tenant_id, draft_id, draft_revision, draft_edited_at, draft_content_hash, \
                catalog_id, catalog_version, environment, flow_id, runtime_flow_version, \
                graph_json, graph_hash, draft_artifact_hash, execution_bundle_hash, \
                binding_base_artifact_hash, validated_draft_hash) \
             VALUES ($1, 'draft-shipping', 1, now(), $2, $3, $4, $5, $6, $7, '{}'::jsonb, \
                     $2, $2, $8, $9, $10)",
            &[
                &TENANT,
                &DRAFT_ARTIFACT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &ENVIRONMENT,
                &CALLEE_FLOW,
                &CANDIDATE_FLOW_VERSION,
                &bundle_hash,
                // The released base: the artifact the candidate's connection
                // requirements were resolved under, NOT the draft's own.
                &CALLEE_ARTIFACT,
                &VALIDATED_DRAFT_HASH,
            ],
        )
        .await
        .expect("seed the validated candidate draft");
}

// ---------------------------------------------------------------------------
// The mint, run as the store owner.
// ---------------------------------------------------------------------------

fn registrations() -> BTreeMap<String, ServingRegistration> {
    BTreeMap::from([(
        "orders-changed".to_string(),
        ServingRegistration {
            flow_id: CALLEE_FLOW.to_string(),
            entity: "orders".to_string(),
            ops: BTreeSet::from(["insert".to_string(), "update".to_string()]),
        },
    )])
}

/// Mint in its own owner-role transaction, exactly as production would.
async fn mint(
    client: &mut Client,
    request: &MintReleaseManifest<'_>,
) -> Result<MintedReleaseManifest, wamn_ctl::publish_release::MintManifestError> {
    // Drop the seeding session's tenant claim first. Leaving it set would let a
    // mint that never claims a tenant of its own still read every row, and the
    // mutant that deletes the claim would then survive — measured, not assumed.
    client
        .execute("SELECT set_config('app.tenant', '', false)", &[])
        .await
        .expect("clear the seeding session's tenant claim");
    let transaction = client.transaction().await.expect("open a mint transaction");
    transaction
        .batch_execute("SET LOCAL ROLE wamn_system")
        .await
        .expect("assume the control store owner");
    let outcome = mint_release_manifest(&transaction, request).await;
    match &outcome {
        Ok(_) => transaction.commit().await.expect("close the mint read"),
        // A refused mint may have left the transaction aborted; never commit one.
        Err(_) => transaction
            .rollback()
            .await
            .expect("close the refused read"),
    }
    outcome
}

#[tokio::test]
async fn the_mint_projects_a_release_and_then_a_candidate_overlay_of_it() {
    let Ok(url) = std::env::var("WAMN_RELEASE_MANIFEST_MINT_PG_URL") else {
        eprintln!(
            "skipping the_mint_projects_a_release_and_then_a_candidate_overlay_of_it \
             (set WAMN_RELEASE_MANIFEST_MINT_PG_URL)"
        );
        return;
    };
    let (mut admin, admin_task) = connect(&url).await;
    provision_control_store(&admin).await;
    seed_release_base(&admin).await;

    let (root_hash, root_bytes) = plan_bytes(&plan(ROOT_ARTIFACT, &[CALLEE_FLOW]));
    let (callee_hash, callee_bytes) = plan_bytes(&plan(CALLEE_ARTIFACT, &[]));
    let (draft_hash, draft_bytes) = plan_bytes(&plan(DRAFT_ARTIFACT, &[]));
    assert_eq!(
        BTreeSet::from([&root_hash, &callee_hash, &draft_hash]).len(),
        3,
        "the three fixtures must be three distinct execution bundles"
    );

    seed_artifact(&admin, ROOT_FLOW, ROOT_FLOW_VERSION, ROOT_ARTIFACT).await;
    seed_artifact(&admin, CALLEE_FLOW, CALLEE_FLOW_VERSION, CALLEE_ARTIFACT).await;
    seed_bundle(&admin, &root_hash, &root_bytes).await;
    seed_bundle(&admin, &callee_hash, &callee_bytes).await;
    seed_bundle(&admin, &draft_hash, &draft_bytes).await;
    seed_member(&admin, ROOT_FLOW, ROOT_FLOW_VERSION, &root_hash).await;
    seed_member(&admin, CALLEE_FLOW, CALLEE_FLOW_VERSION, &callee_hash).await;
    seed_exposure(&admin).await;
    seed_candidate_draft(&admin, &draft_hash).await;

    let supplied = registrations();
    let release_request = MintReleaseManifest {
        tenant_id: TENANT,
        catalog_id: CATALOG_ID,
        catalog_version: CATALOG_VERSION,
        candidate_draft_id: None,
        registrations: &supplied,
    };

    // ---- the released projection ----
    let released = mint(&mut admin, &release_request)
        .await
        .expect("the release projects");
    if std::env::var_os("WAMN_PRINT_RELEASE_MANIFEST_MINT_VECTOR").is_some() {
        eprintln!(
            "MINT_VECTOR_BYTES={}\nMINT_VECTOR_DIGEST={}",
            String::from_utf8(released.canonical_bytes.clone()).expect("manifest bytes are UTF-8"),
            released.digest.as_str()
        );
    }
    let document = &released.manifest;
    assert_eq!(document.format_version, SERVING_MANIFEST_FORMAT_VERSION);
    assert_eq!(
        document.release,
        ServingRelease {
            tenant_id: TENANT.to_string(),
            catalog_id: CATALOG_ID.to_string(),
            catalog_version: 3,
            environment: ENVIRONMENT.to_string(),
        },
        "the environment is derived from the release row, never supplied"
    );

    assert_eq!(
        document.flows.keys().collect::<Vec<_>>(),
        vec![ROOT_FLOW, CALLEE_FLOW]
    );
    let root = &document.flows[ROOT_FLOW];
    assert_eq!(root.flow_version, 1);
    assert_eq!(root.plan_hash, root_hash);
    assert_eq!(root.source_artifact, ROOT_ARTIFACT);
    assert_eq!(root.binding_base_artifact, ROOT_ARTIFACT);
    assert!(root.callable_contract.is_some());
    assert_eq!(root.calls, BTreeSet::from([CALLEE_FLOW.to_string()]));
    let callee = &document.flows[CALLEE_FLOW];
    assert_eq!(callee.flow_version, 2);
    assert_eq!(callee.plan_hash, callee_hash);
    assert_eq!(callee.source_artifact, CALLEE_ARTIFACT);
    assert_eq!(callee.binding_base_artifact, CALLEE_ARTIFACT);
    assert!(callee.calls.is_empty());
    // The call edge is precomputed adjacency, so the reachable set no longer
    // needs plan bytes to walk.
    assert_eq!(
        document.reachable_flows(ROOT_FLOW),
        BTreeSet::from([ROOT_FLOW.to_string(), CALLEE_FLOW.to_string()])
    );

    assert_eq!(
        document.attachments.keys().collect::<Vec<_>>(),
        ["orders-http"]
    );
    let attachment = &document.attachments["orders-http"];
    assert_eq!(attachment.kind, wamn_catalog::AttachmentKind::Http);
    assert_eq!(attachment.flow_id, ROOT_FLOW);
    assert_eq!(attachment.definition_hash, DEFINITION_HASH);
    assert_eq!(attachment.definition["id"], "orders-http");
    // The resolved SOURCE document, not the attachment's own.
    assert_eq!(attachment.auth_policy, serde_json::json!({"mode": "none"}));

    assert_eq!(document.registrations, supplied);

    assert_eq!(
        released.canonical_bytes.as_slice(),
        mint_vector::CANONICAL_BYTES,
        "the real mint no longer produces the reader's pinned identity vector"
    );
    assert_eq!(
        released.digest.as_str(),
        mint_vector::DIGEST,
        "the real mint and reader no longer name the shared vector identically"
    );

    // The bytes admit through the one reader entry point, and the digest they
    // derive is the one the mint handed back.
    assert_eq!(
        ServingManifest::from_canonical_bytes(&released.canonical_bytes),
        Ok((document.clone(), released.digest.clone()))
    );

    // ---- a repeated mint over identical content ----
    let repeated = mint(&mut admin, &release_request)
        .await
        .expect("the release projects again");
    assert_eq!(repeated.canonical_bytes, released.canonical_bytes);
    assert_eq!(repeated.digest, released.digest);

    // ---- the candidate overlay ----
    let candidate = mint(
        &mut admin,
        &MintReleaseManifest {
            candidate_draft_id: Some(VALIDATED_DRAFT_HASH),
            ..release_request
        },
    )
    .await
    .expect("the candidate overlay projects");
    let overlaid = &candidate.manifest.flows[CALLEE_FLOW];
    assert_eq!(overlaid.flow_version, 3, "the draft's runtime flow version");
    assert_eq!(overlaid.plan_hash, draft_hash, "the draft's own plan");
    assert_eq!(
        overlaid.source_artifact, DRAFT_ARTIFACT,
        "source-artifact is the plan-integrity anchor and must be the draft's own \
         artifact, or the plan verifiers refuse the snapshot"
    );
    assert_eq!(
        overlaid.binding_base_artifact, CALLEE_ARTIFACT,
        "binding-base-artifact is the released base the draft was validated against; \
         a draft artifact can never own a connection-binding row"
    );
    assert_ne!(
        overlaid.source_artifact, overlaid.binding_base_artifact,
        "the two duties demand different values for a candidate — that divergence \
         is the whole reason the field exists"
    );
    // The overlay replaces exactly one member and leaves the rest alone.
    assert_eq!(candidate.manifest.flows[ROOT_FLOW], *root);
    assert_ne!(candidate.digest, released.digest);
    assert_eq!(
        ServingManifest::from_canonical_bytes(&candidate.canonical_bytes)
            .expect("the candidate's bytes are admitted")
            .1,
        candidate.digest
    );

    // ---- refusals ----
    let absent = mint(
        &mut admin,
        &MintReleaseManifest {
            catalog_version: CATALOG_VERSION + 1,
            ..release_request
        },
    )
    .await
    .expect_err("a coordinate with no release refuses");
    assert_eq!(absent.kind(), MintManifestErrorKind::Release, "{absent}");

    // The tenant claim is what makes the mint see anything at all under FORCE
    // ROW LEVEL SECURITY; a foreign tenant reads no release identity row and
    // refuses rather than projecting an empty release.
    let foreign = mint(
        &mut admin,
        &MintReleaseManifest {
            tenant_id: "some-other-tenant",
            ..release_request
        },
    )
    .await
    .expect_err("another tenant's coordinate refuses");
    assert_eq!(foreign.kind(), MintManifestErrorKind::Release, "{foreign}");

    let unknown_draft = mint(
        &mut admin,
        &MintReleaseManifest {
            candidate_draft_id: Some(
                "sha256:9999999999999999999999999999999999999999999999999999999999999999",
            ),
            ..release_request
        },
    )
    .await
    .expect_err("an absent candidate draft refuses");
    assert_eq!(
        unknown_draft.kind(),
        MintManifestErrorKind::CandidateDraft,
        "{unknown_draft}"
    );

    drop(admin);
    let _ = admin_task.await;
}
