use super::super::pool::ResolvedCredential;
use super::super::statements::{StatementField, StatementValueType, VerifiedStatement};
use super::transactions::{CLAIM_SQL, causation_emit_sql};
use super::*;
use tokio_postgres::NoTls;

fn candidate_binding(component: &str, alias: &str) -> serde_json::Value {
    serde_json::json!({
        "component-digest": component,
        "store-alias": alias,
        "requirement-hash": "sha256:req",
        "instance-id": "orders",
        "instance-revision": 3,
        "requirement-type": "http",
        "contract": "wamn:connection/http@0.1.0",
        "validation-hash": "sha256:validation",
        "generation": 7,
        "definition-hash": "sha256:definition",
        "credential-set-handle": "orders-7"
    })
}

#[test]
fn candidate_binding_world_requires_complete_canonical_unique_rows() {
    let first = candidate_binding("sha256:a", "primary");
    let second = candidate_binding("sha256:b", "primary");
    let world =
        CandidateBindingWorld::from_json(serde_json::json!([first.clone(), second.clone()]))
            .expect("ordered complete world");
    assert!(world.binding("sha256:a", "primary").is_some());
    assert!(CandidateBindingWorld::from_json(serde_json::json!([second, first])).is_err());
    let mut incomplete = candidate_binding("sha256:a", "primary");
    incomplete
        .as_object_mut()
        .expect("fixture object")
        .remove("generation");
    assert!(CandidateBindingWorld::from_json(serde_json::json!([incomplete])).is_err());
}

#[test]
fn candidate_effect_snapshot_pins_instance_and_generation_without_fallback() {
    for predicate in [
        "($10::text IS NULL OR binding.instance_id = $10)",
        "generation.generation = COALESCE($11::bigint, instance.active_generation)",
        "($11::bigint IS NULL OR instance.active_generation = $11)",
    ] {
        assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains(predicate));
    }
}

/// Building a pool must REQUIRE a probeable credential.
///
/// Without this, removing the exactness hook from `build_pool` would be an
/// inert change: every other test constructs the hook directly, so nothing
/// would notice the pool no longer carries it. Here the hook's construction
/// is the only thing that can reject this url, so its absence is visible.
#[test]
fn building_a_pool_requires_a_probeable_credential() {
    let unprobeable = ResolvedCredential {
        // Parses as a manager config, but names no principal, so the
        // exactness hook cannot be built for it.
        database_url: "postgres://host:5432/db".to_string(),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
        row_limit: 10,
    };
    assert!(
        WamnPostgres::build_pool(&unprobeable, AuthorityClass::GuestSql, DEFAULT_PROJECT)
            .is_err(),
        "a pool whose credential cannot be probed must not be built"
    );
}

/// Guest-sql must never be usable as a platform authority, and the refusal
/// must survive `--release`, where a `debug_assert` would not exist.
#[tokio::test]
async fn a_platform_checkout_refuses_the_guest_authority() {
    let postgres = WamnPostgres::from_env(Some(ClassCredentials::every_class(
        "postgres://wamn_app_refusal_a@localhost/refusal-proof",
    )))
    .expect("compose");
    let refused = postgres
        .checkout_platform(DEFAULT_PROJECT, AuthorityClass::GuestSql)
        .await;
    assert!(
        refused.is_err(),
        "platform work under the guest authority must fail closed"
    );
}

/// REGRESSION GUARD (`wamn-0h0g.22.8.3`).
///
/// Removing the ambient read in split B silently cut the HOST's default
/// project, because `from_env` derived it from the config field that split
/// had just forced to `None`. Nothing in the sweep exercised that path, so
/// the only thing standing between that and a deployed host with no
/// database is this test. It asserts the composed credential actually
/// reaches resolution, and that composing without one resolves NOTHING
/// rather than falling back.
/// A guest credential url for `tenant` in `database`, named the way
/// provisioning names one: the login carries the tenant key as its scope
/// digest (`wamn-0h0g.22.6.4`).
fn guest_url(tenant: &str, database: &str) -> String {
    format!(
        "postgres://wamn_app_{}_a:pw@db/{database}",
        wamn_run_state::app_scope_hash(tenant, database)
    )
}

/// *** THE REFUSAL THAT MAKES THE CREDENTIAL THE AUTHORITY. ***
///
/// After `wamn-0h0g.22.6` a guest's tenant is its LOGIN, so handing back a
/// credential minted for another tenant is not a mis-selection, it is a
/// cross-tenant read. Resolution refuses instead, and refuses again when the
/// caller names no tenant at all.
#[test]
fn a_guest_credential_resolves_for_its_own_tenant_and_no_other() {
    let host = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
        "acme",
        "host-default",
    ))))
    .expect("compose with a credential");
    assert!(
        host.provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
            .expect("the credential's own tenant resolves")
            .is_some()
    );
    assert!(
        host.provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("evil"))
            .is_err(),
        "a credential minted for another tenant must be REFUSED, not borrowed"
    );
    assert!(
        host.provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, None)
            .is_err(),
        "guest resolution without a tenant has no authority to check"
    );
    // Platform classes are project-environment scoped, so the tenant is not
    // part of their binding and its absence is not an error.
    assert!(
        host.provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform, None)
            .expect("platform resolution needs no tenant")
            .is_some()
    );
}

#[test]
fn the_composed_credential_becomes_the_default_project() {
    let composed = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
        "acme",
        "host-default",
    ))))
    .expect("compose with a credential");
    assert!(
        composed
            .provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
            .expect("resolve default")
            .is_some(),
        "a host composed WITH a credential must resolve the default project; \
             deploy/platform injects it via secretKeyRef and a host that cannot \
             resolve it has no database at all"
    );

    let bare = WamnPostgres::from_env(None).expect("compose without a credential");
    assert!(
        bare.provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
            .expect("resolve default")
            .is_none(),
        "composing without a credential must resolve nothing, not reach for \
             an ambient one"
    );
}

#[test]
fn a_serving_host_binds_its_credential_to_the_declared_project() {
    let executor_platform_url = "postgres://executor-platform@db/host-receiving";
    let composed = WamnPostgres::from_env_for_project(
        "receiving",
        Some(
            ClassCredentials::default()
                .with_class(AuthorityClass::ExecutorPlatform, executor_platform_url),
        ),
    )
    .expect("compose the declared project credential");
    let resolved = composed
        .provider
        .resolve("receiving", AuthorityClass::ExecutorPlatform, None)
        .expect("resolve the declared project")
        .expect("the declared project has an executor-platform credential");
    assert_eq!(
        resolved.database_url, executor_platform_url,
        "the host's exact credential must resolve under its trusted project"
    );
    assert!(
        composed
            .provider
            .resolve(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform, None)
            .expect("resolve the unrelated default project")
            .is_none(),
        "binding a named project must not mint a default-project alias"
    );
}

#[test]
fn a_serving_host_refuses_an_invalid_declared_project() {
    let error = WamnPostgres::from_env_for_project("receiving.prod", None)
        .err()
        .expect("an invalid project must refuse before reading ambient configuration");
    assert!(error.to_string().contains("invalid composed project"));
}

#[test]
fn an_explicit_project_credential_refuses_a_second_source() {
    let cfg = WamnPostgresConfig::from_env();
    let credentials = ClassCredentials::every_class(guest_url("acme", "host-receiving"));
    let projects = HashMap::from([(
        "receiving".to_owned(),
        ProjectConfig::from_global(credentials.clone(), &cfg),
    )]);
    let error = bind_composed_project(projects, "receiving", Some(credentials), &cfg)
        .expect_err("two credential sources for one project must refuse");
    assert!(
        error.to_string().contains(
            "both an explicit composition credential and a WAMN_PG_PROJECTS_FILE entry"
        )
    );
}

#[test]
fn guest_and_platform_pool_caches_remain_distinct_under_interleaving() {
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        // wamn-0h0g.22.8.2: a provisioned credential NAMES ITS GENERATION ROLE,
        // and the pool key is derived from it. A url with no user carries no
        // credential identity and is now refused, so this fixture names one
        // rather than relying on a libpq-style implicit OS user.
        credentials: Some(ClassCredentials::every_class(format!(
            "postgres://wamn_app_{}_a@localhost/pool-lifecycle-proof",
            wamn_run_state::app_scope_hash("acme", "pool-lifecycle-proof")
        ))),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
        row_limit: 10,
    })
    .expect("construct lazy lifecycle pools");

    let guest_first = postgres
        .ensure_pool(AuthorityClass::GuestSql, DEFAULT_PROJECT, Some("acme"))
        .expect("first guest pool");
    let platform_first = postgres
        .ensure_pool(AuthorityClass::ExecutorPlatform, DEFAULT_PROJECT, None)
        .expect("first platform pool");
    let platform_second = postgres
        .ensure_pool(AuthorityClass::ExecutorPlatform, DEFAULT_PROJECT, None)
        .expect("memoized platform pool");
    let guest_second = postgres
        .ensure_pool(AuthorityClass::GuestSql, DEFAULT_PROJECT, Some("acme"))
        .expect("memoized guest pool");

    assert!(Arc::ptr_eq(&guest_first, &guest_second));
    assert!(Arc::ptr_eq(&platform_first, &platform_second));
    assert!(!Arc::ptr_eq(&guest_first, &platform_first));

    let mut lifecycle_labels = postgres
        .pool_status_all_by_lifecycle()
        .into_iter()
        .map(|(lifecycle, project, _)| (lifecycle.label(), project))
        .collect::<Vec<_>>();
    lifecycle_labels.sort_unstable();
    assert_eq!(
        lifecycle_labels,
        [
            ("guest", DEFAULT_PROJECT.to_string()),
            ("platform", DEFAULT_PROJECT.to_string()),
        ]
    );
}

// The claim-time record must match `runs_release_record_check` exactly, or a
// claim that would otherwise succeed dies on a CHECK inside the lease grant.
// The hand-rolled shape check this used to exercise is retired: the invariant
// now rides `ManifestDigest`, so what is pinned here is that the TYPE admits
// exactly what the run-plane CHECK admits. Same coupling, one owner.
#[test]
fn manifest_digest_shape_matches_the_run_plane_check() {
    assert!(ManifestDigest::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
    assert!(ManifestDigest::parse(format!("sha256:{}b", "af9".repeat(21))).is_ok());
    for rejected in [
        String::new(),
        "sha256:".to_string(),
        "deadbeef".to_string(),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "a".repeat(65)),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
        format!("SHA256:{}", "a".repeat(64)),
    ] {
        assert!(
            ManifestDigest::parse(rejected.clone()).is_err(),
            "accepted {rejected:?}"
        );
    }
}

// wamn-cjv.2 — the in-band claim/role mutation guard.
#[test]
fn guard_rejects_set_and_reset_variants() {
    for s in [
        "SET app.tenant = 'victim'",
        "set local app.tenant = 'victim'",
        "SET SESSION app.tenant TO 'victim'",
        "SET ROLE postgres",
        "set session authorization postgres",
        // wamn-0h0g.23.1 — the two claims CLAIM_SQL now injects. Overriding
        // either is the privilege escalation the binding would otherwise open:
        // `app.role` clears an exempt-role gate, `app.user_id` reassigns row
        // ownership.
        "SET app.role = 'admin'",
        "set local app.user_id = '00000000-0000-4000-8000-000000000000'",
        "RESET app.role",
        "RESET app.tenant",
        "RESET ALL",
        "   \n\t SET app.tenant='victim'",
        "/* sneaky */ SET app.tenant='victim'",
        "-- lead\nSET app.tenant='victim'",
    ] {
        assert!(statement_mutates_session(s), "should reject: {s:?}");
        assert!(reject_claim_mutation(s).is_err(), "should reject: {s:?}");
    }
}

#[test]
fn guard_rejects_set_config_anywhere() {
    for s in [
        "SELECT set_config('app.tenant','victim',false)",
        "WITH t AS (SELECT set_config('app.tenant','victim',true)) SELECT 1",
        "select pg_catalog.set_config('app.tenant','victim',false)",
        "SELECT SET_CONFIG('app.tenant','victim',false)",
        "SELECT set_config('app.role','admin',true)",
        "WITH t AS (SELECT set_config('app.user_id','00000000-0000-4000-8000-000000000000',true)) SELECT 1",
    ] {
        assert!(statement_mutates_session(s), "should reject: {s:?}");
    }
}

#[test]
fn guard_allows_normal_statements_and_current_setting() {
    for s in [
        "SELECT count(*) FROM s2.rls_secrets WHERE secret LIKE $1",
        "INSERT INTO t (tenant_id, k) VALUES (current_setting('app.tenant', true), $1)",
        "UPDATE t SET a = 1 WHERE id = $1",
        "SELECT current_setting('app.tenant', true)",
        "SELECT * FROM settings",
        "DELETE FROM assets WHERE id = $1",
    ] {
        assert!(!statement_mutates_session(s), "should allow: {s:?}");
        assert!(reject_claim_mutation(s).is_ok(), "should allow: {s:?}");
    }
}

// l5i9.12.2 — the guest wamn.* logical-message forgery guard.
#[test]
fn guard_rejects_guest_causation_forgery() {
    for s in [
        "SELECT pg_logical_emit_message(true,'wamn.causation','{}')",
        "select PG_LOGICAL_EMIT_MESSAGE(true, 'wamn.causation', $1)",
        "SELECT pg_logical_emit_message_bytea(true,'wamn.anything','\\x00')",
        "/* hide */ SELECT pg_logical_emit_message(false,'wamn.x','y')",
        "WITH t AS (SELECT pg_logical_emit_message(true,'wamn.causation','z')) SELECT 1",
    ] {
        assert!(
            statement_forges_causation(s),
            "should detect forgery: {s:?}"
        );
        assert!(reject_claim_mutation(s).is_err(), "should reject: {s:?}");
    }
}

#[test]
fn guard_allows_non_wamn_logical_messages_and_normal_sql() {
    for s in [
        // a guest's OWN (non-reserved) logical message is fine — the reader
        // only stitches `wamn.causation`.
        "SELECT pg_logical_emit_message(true,'app.audit','{}')",
        "SELECT count(*) FROM wamn_things WHERE id = $1",
        "INSERT INTO t (k) VALUES ($1)",
    ] {
        assert!(!statement_forges_causation(s), "should allow: {s:?}");
        assert!(reject_claim_mutation(s).is_ok(), "should allow: {s:?}");
    }
}

// l5i9.12.2 — the emit bytes are the load-bearing contract with the reader
// (l5i9.12.1 parses `wamn.causation` via serde `deny_unknown_fields`), so pin
// them exactly. A builder mutation that drops the message, flips
// `transactional`, or reshapes the JSON must fail this.
#[test]
fn causation_emit_sql_pins_the_transactional_wamn_message() {
    let c = Causation {
        run: "r-1".into(),
        root: "r-1".into(),
        depth: 0,
    };
    assert_eq!(
        causation_emit_sql(&c),
        " SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"r-1\",\"root\":\"r-1\",\"depth\":0}');"
    );
}

#[test]
fn causation_emit_sql_escapes_single_quotes_in_the_run_id() {
    // A run id with a single quote must not break the SQL literal: quotes are
    // doubled (injection-safe), the JSON itself is unchanged.
    let c = Causation {
        run: "o'brien".into(),
        root: "o'brien".into(),
        depth: 2,
    };
    assert_eq!(
        causation_emit_sql(&c),
        " SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"o''brien\",\"root\":\"o''brien\",\"depth\":2}');"
    );
}

// R2/R16 — the claim statement is a FIXED, fully-bound SELECT: every value is
// a `$n` bind, there is no interpolation path. Pin its shape so a regression
// that reintroduces `SET LOCAL` string-building or drops a claim fails here
// (the unit-level twin of the "no `format!` with `SET LOCAL`" grep-gate).
#[test]
fn claim_sql_is_fully_bound_with_no_interpolation() {
    assert!(
        !CLAIM_SQL.to_ascii_uppercase().contains("SET LOCAL"),
        "CLAIM_SQL must not use SET LOCAL"
    );
    for frag in [
        "set_config('app.tenant', $1, true)",
        "set_config('statement_timeout', $2, true)",
        "set_config('search_path', COALESCE($3, current_setting('search_path')), true)",
        "set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true)",
        // wamn-0h0g.23.1 — bound, and bound to the FLOOR when absent: a
        // COALESCE-to-current here would carry a pooled connection's leftover
        // role into the next component's transaction.
        "set_config('app.role', $5, true)",
        "set_config('app.user_id', $6, true)",
    ] {
        assert!(CLAIM_SQL.contains(frag), "CLAIM_SQL missing {frag:?}");
    }
}

#[test]
fn effect_authority_resolves_exact_wiring_component_and_store_alias() {
    for required in [
        "FROM catalog.effective_release_packages AS member",
        "JOIN catalog.wirings AS wiring",
        "member.effective_release_id = $3",
        "member.package_id = $12",
        "wiring.package_id = member.package_id",
        "wiring.package_version = member.package_version",
        "wiring.wiring_id = $5",
        "wiring.version = $6",
        "wiring.graph_json ->> 'wiring-id' = $5",
        "component.tenant_id = executing_member.tenant_id",
        "component.package_id = executing_member.package_id",
        "component.package_version = executing_member.package_version",
        "component.component_digest = $8",
        "component.operations #>> ARRAY[$18, 'registered-operation']",
        "wiring.graph_json #> ARRAY['nodes', $7]",
        "requirement.component_digest = $8",
        "requirement.store_alias = $9",
        "binding.effective_release_id = $3",
        "binding.environment = $4",
    ] {
        assert!(
            CONNECTION_EFFECT_SNAPSHOT_SQL.contains(required),
            "effect authority snapshot omits {required:?}"
        );
    }
}

#[test]
fn effect_authority_has_no_run_plan_frame_or_legacy_requirement_fallback() {
    let sql = CONNECTION_EFFECT_SNAPSHOT_SQL.to_ascii_lowercase();
    for retired in [
        "wamn_run.",
        " from runs ",
        "effect_attempts",
        "execution_bundles",
        "plan_hash",
        "frame_id",
        "flow_id",
        "artifact_hash =",
        "requirement_name =",
        "catalog_id",
        "catalog_version",
        "gated_catalog_version",
        "catalog_heads",
    ] {
        assert!(!sql.contains(retired), "effect authority retains {retired}");
    }
    for write in [" insert ", " update ", " delete "] {
        assert!(!sql.contains(write), "effect authority performs {write:?}");
    }
}

// R16 — the validators stay as the identity-format contract (demoted from the
// injection boundary by R2): a malformed identity fails closed even though
// the value would bind as inert data.
#[test]
fn validate_claims_rejects_malformed_identities() {
    const U1: &str = "11111111-1111-4111-8111-111111111111";
    assert!(
        validate_claims(
            "acme",
            Some("public"),
            Some("owner-1"),
            Some("inspector"),
            Some(U1)
        )
        .is_ok()
    );
    assert!(validate_claims("acme", None, None, None, None).is_ok());
    assert!(validate_claims("bad'tenant", None, None, None, None).is_err());
    assert!(validate_claims("acme", Some("has-hyphen"), None, None, None).is_err());
    assert!(validate_claims("acme", None, Some("bad;runner"), None, None).is_err());
    // `''` is the deny floor, never a claim.
    assert!(validate_claims("acme", None, None, Some(""), None).is_err());
    // A non-uuid user id would raise 22P02 inside every ownership predicate.
    assert!(validate_claims("acme", None, None, None, Some("not-a-uuid")).is_err());
    assert!(validate_claims("acme", None, None, None, Some(&U1[..35])).is_err());
    assert!(validate_claims("acme", None, None, None, Some(&format!("{U1}-1"))).is_err());
}

#[test]
fn set_and_clear_current_run_is_per_component() {
    let pg =
        WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)));
    assert!(pg.current_run_for("c1").is_none());
    pg.set_current_run(
        "c1",
        Some(Causation {
            run: "r1".into(),
            root: "r1".into(),
            depth: 0,
        }),
    );
    assert_eq!(pg.current_run_for("c1").unwrap().run, "r1");
    // a second component is independent.
    assert!(pg.current_run_for("c2").is_none());
    // None clears it.
    pg.set_current_run("c1", None);
    assert!(pg.current_run_for("c1").is_none());
}

// R31 — unbind reaps every per-component claim registry plus the closed
// workload-authority discriminator while leaving another workload's
// component untouched; the project-keyed `pools` map is never touched here.
// Keyed by the workload-id prefix (the runtime's builtin convention). An unknown
// workload id is a no-op.
#[test]
fn clear_component_claims_reaps_all_registries_for_the_workload() {
    let pg =
        WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)));
    // Two components under workload "wl-a", one under "wl-b".
    for c in ["wl-a-component-0", "wl-b-component-0"] {
        pg.set_tenant(c, "acme").unwrap();
        pg.set_project(c, "proj").unwrap();
        pg.set_schema(c, "s_run").unwrap();
        pg.set_runner(c, "owner-1").unwrap();
        pg.bind_workload_authority(c, "event-materializer").unwrap();
        pg.set_role(c, "inspector").unwrap();
        pg.set_user_id(c, "6e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b")
            .unwrap();
        pg.set_current_run(
            c,
            Some(Causation {
                run: "r1".into(),
                root: "r1".into(),
                depth: 0,
            }),
        );
    }

    // Unbinding an UNKNOWN workload clears nothing.
    pg.clear_component_claims("wl-unknown");
    assert_eq!(pg.tenant_for("wl-a-component-0").as_deref(), Some("acme"));

    pg.clear_component_claims("wl-a");

    // Every registry emptied for the unbound workload's component.
    assert_eq!(pg.tenant_for("wl-a-component-0"), None);
    // project_for falls back to DEFAULT_PROJECT once the claim is gone.
    assert_eq!(pg.project_for("wl-a-component-0"), DEFAULT_PROJECT);
    assert_eq!(pg.schema_for("wl-a-component-0"), None);
    assert_eq!(pg.runner_for("wl-a-component-0"), None);
    assert_eq!(
        pg.workload_authority_for("wl-a-component-0"),
        AuthorityClass::GuestSql
    );
    assert_eq!(pg.role_for("wl-a-component-0"), None);
    assert_eq!(pg.user_id_for("wl-a-component-0"), None);
    assert!(pg.current_run_for("wl-a-component-0").is_none());

    // The other workload's component is untouched across the board.
    assert_eq!(pg.tenant_for("wl-b-component-0").as_deref(), Some("acme"));
    assert_eq!(pg.project_for("wl-b-component-0"), "proj");
    assert_eq!(pg.schema_for("wl-b-component-0").as_deref(), Some("s_run"));
    assert_eq!(
        pg.runner_for("wl-b-component-0").as_deref(),
        Some("owner-1")
    );
    assert_eq!(
        pg.workload_authority_for("wl-b-component-0"),
        AuthorityClass::EventMaterializer
    );
    assert_eq!(
        pg.role_for("wl-b-component-0").as_deref(),
        Some("inspector")
    );
    assert_eq!(
        pg.user_id_for("wl-b-component-0").as_deref(),
        Some("6e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b")
    );
    assert_eq!(pg.current_run_for("wl-b-component-0").unwrap().run, "r1");
}

// ------------------------------------------------------------------
// Live-PG checks (hermetic; skipped cleanly when no test URL is set).
// Set WAMN_PG_TEST_URL (or WAMN_PG_URL / DATABASE_URL) to a throwaway
// Postgres. Each test creates + drops its own objects.
// ------------------------------------------------------------------

fn test_pg_url() -> Option<String> {
    std::env::var("WAMN_PG_TEST_URL")
        .or_else(|_| std::env::var("WAMN_PG_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
}

/// The tenant every live guest checkout in this module authenticates for.
const LIVE_TENANT: &str = "acme";

/// Ensure the stable guest ACL role exists, race-tolerantly.
///
/// EVERY guest connection is checked by the credential-exactness hook
/// (`wamn-0h0g.22.8.4`), which requires the session to be a MEMBER of
/// `wamn_app` — so on a fresh cluster, where no cluster-wide role exists
/// yet, a guest checkout fails before it reaches anything under test. That
/// is the production shape (a generation inherits the stable ACL role), so
/// the fixtures reproduce it rather than weaken the hook.
const ENSURE_GUEST_ACL_ROLE_SQL: &str = "DO $acl$ BEGIN \
           BEGIN CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS; \
           EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
         END $acl$;";

fn live_database(admin_url: &str) -> String {
    url::Url::parse(admin_url)
        .expect("parse the live test url")
        .path()
        .trim_start_matches('/')
        .to_string()
}

/// Rewrite a disposable-database URL onto a PROPERLY NAMED guest generation,
/// creating the login if it is missing.
///
/// `wamn-0h0g.22.6.7` binds a guest credential to its tenant: resolution
/// verifies that the login carries `app_scope_hash(tenant, database)`, so a
/// URL naming an arbitrary user no longer resolves for the guest class.
/// That is the point — a shared login is exactly what item 2 retires — and
/// it means a live guest test has to authenticate as a real generation.
async fn live_guest_url(admin_url: &str, tenant: &str) -> String {
    let mut url = url::Url::parse(admin_url).expect("parse the live test url");
    let database = live_database(admin_url);
    let role = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(tenant, &database)
    );
    let admin = connect_raw(admin_url).await;
    admin
        .batch_execute(&format!(
            // The tests in this module run in PARALLEL against one cluster
            // and roles are cluster-wide, so IF NOT EXISTS races: two
            // sessions both see the role absent and both create it. The
            // exception guard is what makes the create idempotent under
            // concurrency, not the existence check.
            "{ENSURE_GUEST_ACL_ROLE_SQL} \
                 DO $$ BEGIN \
                   BEGIN \
                     CREATE ROLE \"{role}\" LOGIN PASSWORD 'live-guest'; \
                   EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
                   END; \
                 END $$; \
                 GRANT wamn_app TO \"{role}\";"
        ))
        .await
        .expect("ensure the live guest generation");
    url.set_username(&role).expect("set the guest login");
    url.set_password(Some("live-guest"))
        .expect("set the password");
    url.to_string()
}

async fn connect_raw(url: &str) -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

fn database_url_for_role(database_url: &str, role: &str, password: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("database URL is an absolute URI");
    url.set_username(role)
        .expect("PostgreSQL URL accepts a username");
    url.set_password(Some(password))
        .expect("PostgreSQL URL accepts a password");
    url.to_string()
}

// R2/R16 — the ACTUAL bound claim statement makes injection-shaped and
// unicode values INERT DATA: bound as `$n`, none takes statement-level effect
// (a marker table a spliced `DROP`/`DELETE` would destroy survives).
// `valid_*` would reject these values, but the point is the BIND is safe
// regardless of validation.
#[tokio::test]
async fn live_bound_claims_are_injection_inert_and_txn_local() {
    let Some(url) = test_pg_url() else {
        return;
    };
    let client = connect_raw(&url).await;
    let marker = format!("wave2_marker_{}", std::process::id());
    client
        .batch_execute(&format!(
            "DROP TABLE IF EXISTS public.{marker}; \
                 CREATE TABLE public.{marker}(id int); \
                 INSERT INTO public.{marker} VALUES (1);"
        ))
        .await
        .unwrap();
    let stmt = client.prepare(CLAIM_SQL).await.unwrap();
    let timeout = "5000";

    // (1) app.tenant / app.runner / app.role / app.user_id are free-form
    //     custom GUCs: injection-shaped
    //     + unicode values bind as DATA and round-trip VERBATIM; the absent
    //     schema ($3 NULL) leaves the server-default search_path untouched.
    let default_sp: Option<String> = client
        .query_one("SELECT current_setting('search_path', true)", &[])
        .await
        .unwrap()
        .get(0);
    let evil_tenant = format!("x'; DROP TABLE public.{marker}; -- 😀Ω");
    let evil_runner = format!("r'; DELETE FROM public.{marker}; --");
    let evil_role = format!("admin'); DROP TABLE public.{marker}; --");
    let evil_user = format!("u'; TRUNCATE public.{marker}; --");
    let no_schema: Option<&str> = None;
    client.batch_execute("BEGIN").await.unwrap();
    let params: [&(dyn ToSql + Sync); 6] = [
        &evil_tenant,
        &timeout,
        &no_schema,
        &evil_runner,
        &evil_role,
        &evil_user,
    ];
    client.execute(&stmt, &params).await.unwrap();

    let got_tenant: Option<String> = client
        .query_one("SELECT current_setting('app.tenant', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(got_tenant.as_deref(), Some(evil_tenant.as_str()));
    let got_runner: Option<String> = client
        .query_one("SELECT current_setting('app.runner', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(got_runner.as_deref(), Some(evil_runner.as_str()));
    let got_role: Option<String> = client
        .query_one("SELECT current_setting('app.role', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(got_role.as_deref(), Some(evil_role.as_str()));
    let got_user: Option<String> = client
        .query_one("SELECT current_setting('app.user_id', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(got_user.as_deref(), Some(evil_user.as_str()));
    let got_sp: Option<String> = client
        .query_one("SELECT current_setting('search_path', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        got_sp, default_sp,
        "absent schema must preserve the default"
    );

    // marker survived — no spliced statement ran.
    let n: i64 = client
        .query_one(&format!("SELECT count(*) FROM public.{marker}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    client.batch_execute("COMMIT").await.unwrap();

    // SET LOCAL equivalence: after COMMIT the txn-local claim is gone. Per the
    // custom-GUC gotcha a touched GUC reverts to '' (NOT NULL) — the value the
    // RLS floor NULLIFs.
    let after: Option<String> = client
        .query_one("SELECT current_setting('app.tenant', true)", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(after.as_deref(), Some(""));

    // (2) The $3 (search_path) bind is a VALUE, not SQL: an injection-shaped
    //     schema is rejected by search_path's own list-check hook (22023) —
    //     parsed as data, never executed — and the marker still stands.
    client.batch_execute("BEGIN").await.unwrap();
    let evil_schema: Option<&str> = Some("s'; DROP TABLE public.foo; --");
    let params2: [&(dyn ToSql + Sync); 6] = [
        &evil_tenant,
        &timeout,
        &evil_schema,
        &evil_runner,
        &evil_role,
        &evil_user,
    ];
    let err = client.execute(&stmt, &params2).await.unwrap_err();
    assert_eq!(
        err.as_db_error().map(|db| db.code().code()),
        Some("22023"),
        "malformed search_path must fail as an invalid VALUE, not execute"
    );
    client.batch_execute("ROLLBACK").await.unwrap();
    let n2: i64 = client
        .query_one(&format!("SELECT count(*) FROM public.{marker}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n2, 1);

    client
        .batch_execute(&format!("DROP TABLE public.{marker}"))
        .await
        .unwrap();
}

// R2/R16 — the REAL plugin path: begin_with_claims injects the guest claim
// set via the bound statement, they are visible in-txn, and revert after the
// txn. `app.tenant` is NOT among them (`wamn-0h0g.22.6.7`): a guest's tenant
// is its LOGIN, and injecting a GUC nothing it can read consults would be a
// second, settable statement about an authority derived elsewhere.
#[tokio::test]
async fn live_begin_with_claims_sets_the_guest_set_without_a_tenant_claim() {
    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(
            live_guest_url(&admin_url, LIVE_TENANT).await,
        )),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    let user_id = "11111111-1111-4111-8111-111111111111";
    let (conn, _pp) = pg
        .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
        .await
        .unwrap();
    pg.begin_with_claims(
        &conn,
        AuthorityClass::GuestSql,
        "acme",
        Some("public"),
        Some("owner-1"),
        Some("inspector"),
        Some(user_id),
        None,
        4321,
    )
    .await
    .unwrap();
    let row = conn
        .query_one(
            "SELECT current_setting('app.tenant', true), \
                 current_setting('statement_timeout', true), \
                 current_setting('search_path', true), \
                 current_setting('app.runner', true), \
                 current_setting('app.role', true), \
                 current_setting('app.user_id', true)",
            &[],
        )
        .await
        .unwrap();
    let tenant: Option<String> = row.get(0);
    let timeout: Option<String> = row.get(1);
    let sp: Option<String> = row.get(2);
    let runner: Option<String> = row.get(3);
    let role: Option<String> = row.get(4);
    let user: Option<String> = row.get(5);
    // THE DELETION, ASSERTED — and NULL is the sharper result. A custom GUC
    // reads back as the EMPTY STRING once it has been set and the SET LOCAL
    // scope ended; it reads NULL only if it was never set in this session at
    // all. So `None` here says more than "the claim was cleared": it says
    // the guest transaction never touched `app.tenant`.
    assert_eq!(
        tenant, None,
        "the guest claim set must NOT inject app.tenant"
    );
    // …and the session it runs on authenticates as the tenant's own
    // generation, which is where its tenant actually comes from.
    let who: String = conn
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        who.contains(&wamn_run_state::app_scope_hash(
            LIVE_TENANT,
            &live_database(&admin_url)
        )),
        "the guest session must authenticate as {LIVE_TENANT}'s generation, got {who:?}"
    );
    assert_eq!(timeout.as_deref(), Some("4321ms"));
    assert_eq!(sp.as_deref(), Some("public"));
    assert_eq!(runner.as_deref(), Some("owner-1"));
    assert_eq!(role.as_deref(), Some("inspector"));
    assert_eq!(user.as_deref(), Some(user_id));

    // COMMIT (the one_shot success path): a `set_config(is_local => true)`
    // claim reverts even across a commit — proving it is truly LOCAL, not a
    // session-level leak.
    conn.batch_execute("COMMIT").await.unwrap();
    let after: Option<String> = conn
        .query_one("SELECT current_setting('app.role', true)", &[])
        .await
        .unwrap()
        .get(0);
    // `app.role` IS injected by the guest set, so after the commit it reads
    // back as the empty string — the reset value, which is what proves the
    // claim was transaction-LOCAL rather than a session-level leak.
    assert_eq!(after.as_deref(), Some(""));
}

/// Rows a component sees for `sql` on the production one-shot path.
async fn visible_rows(pg: &WamnPostgres, component: &str, sql: &str) -> usize {
    match pg.one_shot(component, sql, &[], true).await {
        Ok(OneShotResult::Rows(rows)) => rows.rows.len(),
        Ok(OneShotResult::Count(_)) => unreachable!("one_shot(want_rows) returns rows"),
        Err(e) => panic!("one_shot {sql:?} failed: {e:?}"),
    }
}

/// SCAFFOLDING FLOOR, NOT THE PRODUCTION ONE. Production keys the permissive
/// floor on `current_user` through `wamn_authority.tenant_key`
/// (`wamn-0h0g.22.6`), which needs the authority derivations installed — and
/// those are built by the provisioner, which the shipped runtime
/// deliberately does not link. The floor is not this fixture's subject: the
/// RESTRICTIVE per-role and per-user layer is, and that is a different claim
/// class item 2 leaves alone. The production floor is proven live by
/// `crates/control/provision/tests/deploy_sql_authority.rs` and the static
/// `deploy/sql/app-schema.sql` contract.
///
/// The 3.2 tenant floor plus the row-ownership rule documented by
/// `deploy/sql/app-schema.sql`, over a table the probe role does not own so
/// RLS applies to it.
fn rls_fixture_sql(schema: &str, probe: &str, tenant: &str, u1: &str, u2: &str) -> String {
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DROP ROLE IF EXISTS {probe}; \
             {ENSURE_GUEST_ACL_ROLE_SQL} \
             CREATE ROLE {probe} LOGIN PASSWORD '{probe}' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {probe}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions ( \
                 tenant_id text NOT NULL, id int NOT NULL, inspector_id uuid NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING (tenant_id = '{tenant}'); \
             CREATE POLICY \"dispositions_owner_0\" ON {schema}.dispositions AS RESTRICTIVE \
                 FOR ALL \
                 USING (COALESCE(current_setting('app.role', true), '') IN ('supervisor', 'admin') \
                        OR \"inspector_id\" = NULLIF(current_setting('app.user_id', true), '')::uuid); \
             INSERT INTO {schema}.dispositions VALUES ('{tenant}', 1, '{u1}'), ('{tenant}', 2, '{u2}'); \
             GRANT USAGE ON SCHEMA {schema} TO {probe}; \
             GRANT SELECT ON {schema}.dispositions TO {probe};"
    )
}

// wamn-0h0g.23.1 — the compiled per-user / per-role rules key on `app.role`
// and `app.user_id`, and under their COALESCE / NULLIF deny floors a policy
// that is never handed those claims denies EVERYTHING. Before this bead
// `CLAIM_SQL` bound only tenant / statement_timeout / search_path /
// app.runner, so every per-user policy silently denied on the production
// path while isolated policy tests passed on hand-written `SET LOCAL`.
// This drives the REAL plugin (`one_shot`) as a NOSUPERUSER
// NOBYPASSRLS role, so it fails against a `CLAIM_SQL` that does not inject
// the caller's identity.
#[tokio::test]
async fn live_compiled_per_user_policy_permits_the_injected_caller() {
    const TENANT: &str = "rls-claim-live";
    const COMPONENT: &str = "rls-claim-live-component";
    const U1: &str = "11111111-1111-4111-8111-111111111111";
    const U2: &str = "22222222-2222-4222-8222-222222222222";

    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let suffix = std::process::id();
    let schema = format!("wamn_rls_claim_{suffix}");
    // The probe login is NAMED AS A GUEST GENERATION for this test's tenant:
    // guest credential resolution verifies that the login carries
    // `app_scope_hash(tenant, database)` (`wamn-0h0g.22.6.7`), so a probe
    // with an arbitrary name would be refused before the policy under test
    // ever ran. The two tenants differ per test, so the derived logins do
    // too and the tests stay parallel-safe.
    let probe = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
    );
    let _ = suffix;
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
        .await
        .expect("seed the per-user RLS fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(database_url_for_role(
            &admin_url, &probe, &probe,
        ))),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    pg.set_tenant(COMPONENT, TENANT).unwrap();
    pg.set_schema(COMPONENT, &schema).unwrap();
    pg.set_role(COMPONENT, "inspector").unwrap();
    pg.set_user_id(COMPONENT, U1).unwrap();

    // CONTROL: the table is REACHABLE on this path — an aggregate returns its
    // one row however many rows RLS filtered away, and a wrong search_path or
    // a missing GRANT would raise instead. So a zero below is a policy
    // denying, not a broken fixture.
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT count(*) FROM dispositions").await,
        1,
        "control: the probe role must reach the fixture table"
    );

    // An inspector sees ONLY the row it owns.
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 1").await,
        1,
        "the injected app.user_id must permit the caller's OWN row"
    );
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 2").await,
        0,
        "the ownership rule must still deny another user's row"
    );
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
        1
    );

    // …and an exempt role sees both, through the injected app.role.
    pg.set_role(COMPONENT, "admin").unwrap();
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "the injected app.role must satisfy the exempt-role gate"
    );

    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"
        ))
        .await
        .expect("drop the per-user RLS fixture");
}

// wamn-0h0g.23.1 — the SET-override refusal, on the two claims the fix now
// injects. `reject_claim_mutation` (wamn-cjv.2) is the mechanism the tenant
// claim already carries and it is GUC-agnostic, so it covers these the moment
// they exist — but coverage that is never exercised is not coverage, and a
// binding WITHOUT refusal turns a silent-deny bug into privilege escalation.
// The CONTROL below proves the escalation is real, so the refusal that
// follows is load-bearing rather than vacuous.
#[tokio::test]
async fn live_guest_cannot_override_the_injected_role_or_user_claim() {
    const TENANT: &str = "rls-override-live";
    const COMPONENT: &str = "rls-override-live-component";
    const U1: &str = "11111111-1111-4111-8111-111111111111";
    const U2: &str = "22222222-2222-4222-8222-222222222222";

    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let suffix = std::process::id();
    let schema = format!("wamn_rls_override_{suffix}");
    // The probe login is NAMED AS A GUEST GENERATION for this test's tenant:
    // guest credential resolution verifies that the login carries
    // `app_scope_hash(tenant, database)` (`wamn-0h0g.22.6.7`), so a probe
    // with an arbitrary name would be refused before the policy under test
    // ever ran. The two tenants differ per test, so the derived logins do
    // too and the tests stay parallel-safe.
    let probe = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
    );
    let _ = suffix;
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
        .await
        .expect("seed the per-user RLS fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(database_url_for_role(
            &admin_url, &probe, &probe,
        ))),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    pg.set_tenant(COMPONENT, TENANT).unwrap();
    pg.set_schema(COMPONENT, &schema).unwrap();
    pg.set_role(COMPONENT, "inspector").unwrap();
    pg.set_user_id(COMPONENT, U1).unwrap();

    // CONTROL: inside ONE plugin-managed transaction the injected claims admit
    // the caller's own row — and a bare `SET LOCAL` on that same transaction
    // clears the exempt-role gate and reveals BOTH. So the escalation the
    // guard refuses below is real, not hypothetical.
    let (conn, _pp) = pg.checkout_guest(DEFAULT_PROJECT, TENANT).await.unwrap();
    pg.begin_with_claims(
        &conn,
        AuthorityClass::GuestSql,
        TENANT,
        Some(&schema),
        None,
        Some("inspector"),
        Some(U1),
        None,
        5_000,
    )
    .await
    .unwrap();
    let owned: i64 = conn
        .query_one("SELECT count(*) FROM dispositions", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        owned, 1,
        "control: the injected caller owns exactly one row"
    );
    conn.batch_execute("SET LOCAL app.role = 'admin'")
        .await
        .unwrap();
    let escalated: i64 = conn
        .query_one("SELECT count(*) FROM dispositions", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        escalated, 2,
        "control: an unguarded SET LOCAL app.role IS an escalation"
    );
    conn.batch_execute("ROLLBACK").await.unwrap();

    // The guest surface refuses exactly that, and every shape of it, before it
    // reaches the server — so the caller still sees only its own row.
    for attempt in [
        "SET app.role = 'admin'",
        "SET LOCAL app.role = 'admin'",
        "RESET app.role",
        &format!("SET app.user_id = '{U2}'"),
        "SELECT set_config('app.role', 'admin', true)",
        &format!("SELECT set_config('app.user_id', '{U2}', true)"),
    ] {
        let refused = pg.one_shot(COMPONENT, attempt, &[], false).await;
        assert!(
            matches!(refused, Err(PgError::QueryError(_))),
            "the guest surface must refuse {attempt:?}"
        );
    }
    assert_eq!(
        visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
        1,
        "the caller's claims are unchanged by the refused overrides"
    );

    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"
        ))
        .await
        .expect("drop the per-user RLS fixture");
}

/// wamn-0h0g.17.7 — `ConnectionHttp` freezes its `(tenant, project)` at store
/// construction and cannot be rebound at checkout, so the registry has to
/// refuse it when the two disagree.
///
/// Offline on purpose: the check runs before any pool is reached, so a
/// disagreement is refused with THIS message while agreement falls through
/// to the (absent) connection. Both halves are asserted, because a guard
/// that refused everything would pass the first alone.
#[tokio::test]
async fn effect_snapshot_refuses_a_tenant_that_disagrees_with_the_bound_claim() {
    const COMPONENT: &str = "warm-instance-0";
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
        row_limit: 10,
    })
    .unwrap();
    pg.bind_session_claims(
        COMPONENT,
        &SessionClaims {
            tenant: "tenant-b".to_string(),
            ..SessionClaims::default()
        },
    )
    .expect("the acquiring tenant binds");

    let lookup = ConnectionEffectLookup {
        wiring_package_id: "catalog",
        origin_package_id: "catalog",
        origin_component_digest: "digest",
        origin_component: "component",
        origin_interface_version: "0.1.0",
        origin_operation: "operation",
        package_id: "catalog",
        operation: "operation",
        effective_release_id: 1,
        environment: "dev",
        wiring_id: "wiring",
        wiring_version: 1,
        node_id: "node",
        component_digest: "digest",
        store_alias: "manager",
        candidate_binding: None,
    };

    // A stale ConnectionHttp still carrying the tenant its store was BUILT
    // for is refused before it can read a row.
    let stale = pg
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
        .await
        .expect_err("a disagreeing tenant is refused");
    assert!(
        stale.to_string().contains(
            "HTTP effect authorization tenant \"tenant-a\" disagrees with the tenant bound"
        ),
        "the refusal names the divergence rather than any other failure: {stale}"
    );

    // The agreeing tenant gets past the guard and fails only on the absent
    // connection, so the guard is not simply refusing everything.
    let agreeing = pg
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-b", &lookup)
        .await
        .expect_err("an offline plugin has no connection to resolve against");
    assert!(
        !agreeing
            .to_string()
            .contains("disagrees with the tenant bound"),
        "the bound tenant must pass the guard: {agreeing}"
    );
}

/// wamn-0h0g.17.11 — the guard requires a bound claim, it does not merely
/// refuse disagreement.
///
/// Agreement with the registry carries no information when the registry has
/// no entry, so an unbound claim scope must be refused rather than trusted
/// with whatever tenant the caller froze. This is the same deny floor
/// `require_tenant` applies to every other read: an instance that skipped a
/// bind resolves nothing.
///
/// Offline on purpose: the refusal lands before any pool is reached, and the
/// message is asserted so a mutant that lets the unbound case fall through
/// fails on the message rather than passing on the (also absent) connection.
#[tokio::test]
async fn effect_snapshot_refuses_a_component_with_no_bound_tenant() {
    const COMPONENT: &str = "never-acquired-instance";
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
        row_limit: 10,
    })
    .unwrap();
    assert_eq!(
        pg.session_claims(COMPONENT),
        None,
        "the scope under test resolves no identity at all"
    );

    let lookup = ConnectionEffectLookup {
        wiring_package_id: "catalog",
        origin_package_id: "catalog",
        origin_component_digest: "digest",
        origin_component: "component",
        origin_interface_version: "0.1.0",
        origin_operation: "operation",
        package_id: "catalog",
        operation: "operation",
        effective_release_id: 1,
        environment: "dev",
        wiring_id: "wiring",
        wiring_version: 1,
        node_id: "node",
        component_digest: "digest",
        store_alias: "manager",
        candidate_binding: None,
    };

    let unbound = pg
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
        .await
        .expect_err("an unbound claim scope resolves nothing");
    assert!(
        unbound.to_string().contains(
            "HTTP effect authorization for component \"never-acquired-instance\" has no \
                 bound tenant claim"
        ),
        "the refusal names the missing claim rather than any other failure: {unbound}"
    );
}

/// A provider that RECORDS which authority class each resolution was asked
/// for, and names no credential for any of them.
///
/// `Ok(None)` is what keeps the proof hermetic: `ensure_pool` refuses on the
/// spot, so the class under test is observed with no pool built and no socket
/// opened.
#[derive(Default)]
struct RecordingProvider {
    asked: std::sync::Mutex<Vec<AuthorityClass>>,
}

#[tokio::test]
async fn workload_binding_selects_only_the_materializer_authority() {
    let provider = Arc::new(RecordingProvider::default());
    let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
    pg.bind_workload_authority("materializer", "event-materializer")
        .expect("the closed materializer value is admitted");
    pg.bind_workload_authority("invalid", "executor-platform")
        .expect_err("no other explicit authority is admitted");

    assert!(
        pg.checkout_workload("materializer", DEFAULT_PROJECT, "tenant-a")
            .await
            .is_err(),
        "the recording provider deliberately names no credential"
    );
    assert!(
        pg.checkout_workload("ordinary-guest", DEFAULT_PROJECT, "tenant-a")
            .await
            .is_err(),
        "the recording provider deliberately names no credential"
    );

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned")
        .clone();
    assert_eq!(
        asked,
        vec![AuthorityClass::EventMaterializer, AuthorityClass::GuestSql]
    );
}

impl CredentialProvider for RecordingProvider {
    fn resolve(
        &self,
        _project: &str,
        class: AuthorityClass,
        _tenant: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        self.asked
            .lock()
            .expect("recording provider lock poisoned")
            .push(class);
        Ok(None)
    }
}

/// THE TRUSTED HTTP EFFECT CHECKS OUT UNDER `CallableHttp` AND NOTHING ELSE
/// (`wamn-0h0g.22.11`).
///
/// THE COVERAGE THIS CLOSES. Until this test the callable-HTTP checkout class
/// had NO coverage at all: a mutant making this method check out
/// `AuthorityClass::ExecutorPlatform` instead was INERT — nothing in the
/// crate distinguished it. The three offline `connection_effect_snapshot`
/// tests refuse before a pool is reached, and `checkout_platform` maps every
/// failure to the one `PgError::ConnectionUnavailable`, so no error message
/// can name the class either. The provider is the seam where the class IS
/// observable: it is the exact argument `ensure_pool` forwards, so recording
/// it pins the production routing rather than restating it.
///
/// Sequence equality, not containment. `checkout_platform` refuses
/// `AuthorityClass::GuestSql` BEFORE consulting the provider, so a mutant
/// naming the guest records nothing at all; asserting the whole sequence
/// kills that arm too, along with any second checkout under another class.
#[tokio::test]
async fn effect_snapshot_checks_out_under_the_callable_http_authority() {
    const COMPONENT: &str = "warm-instance-0";
    let provider = Arc::new(RecordingProvider::default());
    let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
    pg.bind_session_claims(
        COMPONENT,
        &SessionClaims {
            tenant: "tenant-a".to_string(),
            ..SessionClaims::default()
        },
    )
    .expect("the acquiring tenant binds");

    let lookup = ConnectionEffectLookup {
        wiring_package_id: "catalog",
        origin_package_id: "catalog",
        origin_component_digest: "digest",
        origin_component: "component",
        origin_interface_version: "0.1.0",
        origin_operation: "operation",
        package_id: "catalog",
        operation: "operation",
        effective_release_id: 1,
        environment: "dev",
        wiring_id: "wiring",
        wiring_version: 1,
        node_id: "node",
        component_digest: "digest",
        store_alias: "manager",
        candidate_binding: None,
    };

    let refused = pg
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
        .await
        .expect_err("a provider that names no credential resolves nothing");
    assert!(
        !refused
            .to_string()
            .contains("disagrees with the tenant bound"),
        "the bound tenant must reach the checkout, or the class below is \
             never asked for: {refused}"
    );

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned")
        .clone();
    assert_eq!(
        asked,
        vec![AuthorityClass::CallableHttp],
        "the callable-HTTP authority snapshot must check out as the \
             callable-HTTP family and no other"
    );
}

#[tokio::test]
async fn operation_permissions_reuse_only_the_callable_http_authority() {
    let provider = Arc::new(RecordingProvider::default());
    let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);

    pg.operation_permissions(DEFAULT_PROJECT, "tenant-a", "route-caller")
        .await
        .expect_err("a provider that names no credential resolves nothing");

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned")
        .clone();
    assert_eq!(asked, vec![AuthorityClass::CallableHttp]);
}

#[tokio::test]
async fn user_operation_permissions_reuse_only_the_callable_http_authority() {
    let provider = Arc::new(RecordingProvider::default());
    let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
    let principal_id = "00000000-0000-0000-0000-000000000019"
        .parse()
        .expect("user UUID");

    pg.user_operation_permissions(DEFAULT_PROJECT, "tenant-a", &principal_id)
        .await
        .expect_err("a provider that names no credential resolves nothing");

    let asked = provider
        .asked
        .lock()
        .expect("recording provider lock poisoned")
        .clone();
    assert_eq!(asked, vec![AuthorityClass::CallableHttp]);
}

/// The tenant floor over rows belonging to TWO tenants, keyed on
/// `current_user` — the shape `wamn-0h0g.22.6` put in production.
///
/// The literal role names stand in for `wamn_authority.tenant_key`, which
/// this crate cannot install (the derivations are built by the provisioner,
/// which the shipped runtime deliberately does not link). What the fixture
/// reproduces faithfully is the thing under test: the row filter reads the
/// CONNECTED ROLE, so a session cannot talk its way into another tenant's
/// rows — there is no claim to rewrite.
fn two_tenant_rls_fixture_sql(
    schema: &str,
    role_a: &str,
    role_b: &str,
    a: &str,
    b: &str,
) -> String {
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role_a}') THEN \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a}; END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role_b}') THEN \
                 DROP OWNED BY {role_b}; DROP ROLE {role_b}; END IF; \
             END $reset$; \
             {ENSURE_GUEST_ACL_ROLE_SQL} \
             CREATE ROLE {role_a} LOGIN PASSWORD 'live-guest' NOSUPERUSER NOBYPASSRLS; \
             CREATE ROLE {role_b} LOGIN PASSWORD 'live-guest' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {role_a}, {role_b}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions (tenant_id text NOT NULL, id int NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING ((tenant_id = '{a}' AND current_user = '{role_a}') \
                     OR (tenant_id = '{b}' AND current_user = '{role_b}')); \
             INSERT INTO {schema}.dispositions \
                 VALUES ('{a}', 1), ('{a}', 2), ('{b}', 3); \
             GRANT USAGE ON SCHEMA {schema} TO {role_a}, {role_b}; \
             GRANT SELECT ON {schema}.dispositions TO {role_a}, {role_b};"
    )
}

/// *** THE ADVERSARIAL ARM, RE-EXPRESSED ON THE NEW MECHANISM. ***
///
/// This test used to prove that two interleaved CLAIM sets each saw only
/// their own rows. That subject is retired: after `wamn-0h0g.22.6` a guest's
/// tenant is its LOGIN, and under the owner ruling on `wamn-0h0g.22.6.7` a
/// host holds ONE guest credential per project-environment. So the property
/// worth proving is stronger and simpler — a second tenant is REFUSED rather
/// than quietly served the credential the host does hold.
///
/// The logins are NOSUPERUSER NOBYPASSRLS, so the server cannot be talked
/// out of the floor either.
#[tokio::test]
async fn live_a_second_tenant_is_refused_rather_than_served_the_first_tenants_rows() {
    const TENANT_A: &str = "seam-live-a";
    const TENANT_B: &str = "seam-live-b";

    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let suffix = std::process::id();
    let schema = format!("wamn_seam_{suffix}");
    let database = live_database(&admin_url);
    // Named exactly as provisioning names a guest generation, so the
    // credential the host resolves is bound to TENANT_A by its own digest.
    let role_a = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT_A, &database)
    );
    let role_b = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT_B, &database)
    );
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&two_tenant_rls_fixture_sql(
            &schema, &role_a, &role_b, TENANT_A, TENANT_B,
        ))
        .await
        .expect("seed the two-tenant RLS fixture as the superuser owner");

    let mut url = url::Url::parse(&admin_url).expect("parse the live test url");
    url.set_username(&role_a).expect("set A's login");
    url.set_password(Some("live-guest"))
        .expect("set A's password");
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(url.to_string())),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();

    let scope_a = "warm-instance-0";
    let scope_b = "warm-instance-1";
    let claims = |tenant: &str| SessionClaims {
        tenant: tenant.to_string(),
        schema: Some(schema.clone()),
        ..SessionClaims::default()
    };
    pg.bind_session_claims(scope_a, &claims(TENANT_A))
        .expect("tenant A's acquisition binds");
    pg.bind_session_claims(scope_b, &claims(TENANT_B))
        .expect("tenant B's acquisition binds");

    // CONTROL: the table is reachable on this path at all, so the refusal
    // below is a refusal and not a broken fixture or search_path.
    assert_eq!(
        visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "A sees exactly its own two rows through its own login"
    );

    // B's acquisition is legitimate; the HOST simply holds no credential for
    // it. That must refuse. Serving A's credential would hand B two rows
    // belonging to another tenant, which is the failure this design exists
    // to make impossible.
    let refused = pg
        .one_shot(scope_b, "SELECT id FROM dispositions", &[], true)
        .await
        .err()
        .expect("a tenant this host holds no credential for cannot query");
    assert!(
        matches!(&refused, PgError::ConnectionUnavailable),
        "a second tenant must be REFUSED at credential resolution, not served \
             the first tenant's connection: {refused:?}"
    );

    assert_eq!(
        visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
        2,
        "A's rows are unchanged by B's refused acquisition"
    );

    // Ending A's checkout revokes A's identity and nothing else. Matched on
    // the NO-TENANT code specifically: a revoke that cleared only the
    // search_path would also fail this query, for an unrelated reason.
    pg.revoke_session_claims(scope_a);
    assert_eq!(pg.session_claims(scope_a), None);
    let unbound = pg
        .one_shot(scope_a, "SELECT id FROM dispositions", &[], true)
        .await
        .err()
        .expect("an unbound claim scope cannot query at all");
    assert!(
        matches!(&unbound, PgError::QueryError((code, _)) if code == "WAMN0"),
        "an instance whose checkout ended must resolve NO tenant, not merely \
             fail for some other reason: {unbound:?}"
    );

    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a}; \
                 DROP OWNED BY {role_b}; DROP ROLE {role_b};"
        ))
        .await
        .expect("drop the fixture");
}

// R18 — the post_create hook runs on connect; a successful checkout from the
// pool proves the assertion passed on this server (stock PG18 = on).
#[tokio::test]
async fn live_connect_asserts_standard_conforming_strings() {
    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(
            live_guest_url(&admin_url, LIVE_TENANT).await,
        )),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    // The checkout builds the pool (with the R18 hook) and creates a physical
    // connection; the hook must pass for this to be Ok.
    let (conn, _pp) = pg
        .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
        .await
        .expect("checkout ok (scs=on)");
    let scs: String = conn
        .query_one("SHOW standard_conforming_strings", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(scs, "on");
}

#[tokio::test]
#[ignore = "requires WAMN_POOL_LIFECYCLE_PG_URL for a disposable PostgreSQL database"]
async fn live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving() {
    let admin_url = std::env::var("WAMN_POOL_LIFECYCLE_PG_URL")
        .expect("set WAMN_POOL_LIFECYCLE_PG_URL to a disposable PostgreSQL database");
    let url = live_guest_url(&admin_url, LIVE_TENANT).await;
    let database = live_database(&admin_url);
    let family = wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform;
    let role = wamn_control_provision::workload_generation_role(
        family,
        wamn_control_provision::WorkloadRoleScope::ProjectEnvironment {
            org: "claims-test",
            project: DEFAULT_PROJECT,
            environment: "lifecycle",
            database: &database,
        },
        wamn_control_provision::CredentialGeneration::A,
    )
    .expect("derive the executor generation");
    let role_ident = wamn_pg_core::quote_ident(&role);
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&format!(
            "DO $$ BEGIN \
               BEGIN CREATE ROLE {role_ident} LOGIN PASSWORD 'live-platform' \
                 NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
               EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
             END $$; {}",
            wamn_control_provision::sql::normalize_workload_generation_membership_sql(
                family, &role, true,
            ),
        ))
        .await
        .expect("ensure the live executor generation");
    let executor_url = database_url_for_role(&admin_url, &role, "live-platform");
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(
            ClassCredentials::default()
                .with_class(AuthorityClass::GuestSql, url)
                .with_class(AuthorityClass::ExecutorPlatform, executor_url),
        ),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 250,
        statement_timeout_ms: 1_000,
        row_limit: 10,
    })
    .expect("construct size-one lifecycle pools");

    let (guest, _) = postgres
        .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
        .await
        .expect("hold the only guest connection");
    let guest_row = guest
        .query_one(
            "SELECT pg_backend_pid(), \
                 set_config('wamn.pool_lifecycle_probe', 'guest', false)",
            &[],
        )
        .await
        .expect("mark guest session");
    let guest_pid = guest_row.get::<_, i32>(0);

    // This checkout happens while the sole guest slot remains held. Sharing
    // either pool/cache makes it hit the 250 ms wait bound and fail here.
    let (platform, _) = postgres
        .checkout_platform(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform)
        .await
        .expect("platform headroom remains available while guest is saturated");
    let platform_row = platform
        .query_one(
            "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
            &[],
        )
        .await
        .expect("read untouched platform session");
    let platform_pid = platform_row.get::<_, i32>(0);
    let platform_marker = platform_row.get::<_, Option<String>>(1);
    assert_ne!(guest_pid, platform_pid);
    assert!(platform_marker.is_none());
    platform
        .query_one(
            "SELECT set_config('wamn.pool_lifecycle_probe', 'platform', false)",
            &[],
        )
        .await
        .expect("mark platform session");
    drop(platform);
    drop(guest);

    let (guest_again, _) = postgres
        .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
        .await
        .expect("reacquire guest lifecycle");
    let guest_again_row = guest_again
        .query_one(
            "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
            &[],
        )
        .await
        .expect("read guest session after repool");
    assert_eq!(guest_again_row.get::<_, i32>(0), guest_pid);
    assert_eq!(
        guest_again_row.get::<_, Option<String>>(1).as_deref(),
        Some("guest")
    );

    let (platform_again, _) = postgres
        .checkout_platform(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform)
        .await
        .expect("reacquire platform lifecycle");
    let platform_again_row = platform_again
        .query_one(
            "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
            &[],
        )
        .await
        .expect("read platform session after repool");
    assert_eq!(platform_again_row.get::<_, i32>(0), platform_pid);
    assert_eq!(
        platform_again_row.get::<_, Option<String>>(1).as_deref(),
        Some("platform")
    );
}

// R18-neg (wamn-2jkm.65) — the fail-CLOSED branch, exercised against a REAL
// server booted with standard_conforming_strings=off. The positive above
// proves the hook passes on a stock server; this proves it REJECTS an unsafe
// one and that the guest sees `connection-unavailable`. Gated on a SEPARATE
// url (WAMN_SCS_OFF_PG_URL) so it never runs against the stock test server;
// skipped LOUDLY when unset. Recipe: docs/operations/build-and-test.md [R18-NEG].
#[tokio::test]
async fn live_scs_off_server_fails_checkout_closed() {
    let Some(url) = std::env::var("WAMN_SCS_OFF_PG_URL").ok() else {
        eprintln!(
            "WAMN_SCS_OFF_PG_URL unset — skipping the wamn-2jkm.65 R18 live negative \
                 (boot a postgres:18 with -c standard_conforming_strings=off; see \
                 docs/operations/build-and-test.md [R18-NEG])"
        );
        return;
    };

    // CONTROL: the server must be REACHABLE and genuinely report scs=off, so
    // the checkout failure below is the HOOK rejecting a live server, not a
    // dead url or a network-level connect failure. A raw connect that returns
    // "off" proves both — and if the url were dead this connect would panic,
    // so the test cannot false-pass against a server-down url.
    let raw = connect_raw(&url).await;
    let scs: String = raw
        .query_one("SHOW standard_conforming_strings", &[])
        .await
        .expect("control: server reachable for the scs probe")
        .get(0);
    assert_eq!(
        scs, "off",
        "control: point WAMN_SCS_OFF_PG_URL at a server booted with \
             standard_conforming_strings=off (got {scs:?}); otherwise this test is vacuous"
    );

    // The production path: build the plugin exactly as production does and
    // check out. build_pool installs the R18 post_create hook, which runs
    // `SHOW standard_conforming_strings` on the new physical connection and
    // fails the create; checkout maps that pool error to the WIT
    // `connection-unavailable` variant the guest sees.
    // The url names a REAL guest generation, so resolution succeeds and the
    // refusal below can only come from the hook. A url with an arbitrary
    // user would now be refused at RESOLUTION (`wamn-0h0g.22.6.7`) — the same
    // `connection-unavailable` variant for a different reason, which is
    // exactly the false positive this test's second half exists to rule out.
    let guest_url = live_guest_url(&url, LIVE_TENANT).await;
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    // (`matches!`, not `expect_err`, so the Ok type need not be `Debug`.)
    let result = pg.checkout_guest(DEFAULT_PROJECT, LIVE_TENANT).await;
    assert!(
        matches!(result, Err(PgError::ConnectionUnavailable)),
        "scs=off must fail CLOSED as the guest-visible connection-unavailable \
             variant — a checkout that succeeded means the hook did not reject"
    );

    // Hook-SPECIFICITY: reach the raw pool error (which checkout collapses to
    // connection-unavailable) and confirm it is the R18 post_create hook —
    // not an auth/other failure that ALSO maps to connection-unavailable. The
    // control above already ruled out server-down; this pins the cause.
    let pool = WamnPostgres::build_pool(
        &ResolvedCredential {
            database_url: url,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        },
        AuthorityClass::GuestSql,
        DEFAULT_PROJECT,
    )
    // Hook ORDER matters to this test: R18 is pushed first, so a scs=off
    // server fails on standard_conforming_strings before the
    // wamn-0h0g.22.8.4 exactness hook is reached.
    .expect("pool builds (url parses; the hooks run at checkout, not build)");
    let raw_err = match pool.get().await {
        Ok(_) => panic!("raw checkout unexpectedly SUCCEEDED against a scs=off server"),
        Err(e) => e,
    };
    let rendered = raw_err.to_string();
    assert!(
        rendered.contains("standard_conforming_strings"),
        "the pool error must be the R18 fail-closed hook, got: {rendered}"
    );
}

// ------------------------------------------------------------------
// wamn-0h0g.17.33 — the pipelined claim flight, and its proof obligation.
// ------------------------------------------------------------------

/// The fixture a cold-parse check needs: a schema the session's own
/// `search_path` does NOT contain, holding the only relation the statement
/// names, readable by the guest generation.
fn cold_parse_fixture_sql(schema: &str, role: &str) -> String {
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.cold_parse (id int NOT NULL); \
             INSERT INTO {schema}.cold_parse VALUES (1), (2); \
             GRANT USAGE ON SCHEMA {schema} TO \"{role}\"; \
             GRANT SELECT ON {schema}.cold_parse TO \"{role}\";"
    )
}

/// The statement a cold-parse check runs: it names `cold_parse`
/// UNQUALIFIED, so Parse resolves it only under the claimed `search_path`.
fn cold_parse_statement() -> VerifiedStatement {
    VerifiedStatement {
        exact_sql: "SELECT id FROM cold_parse ORDER BY id".into(),
        binds: Box::new([]),
        columns: Box::new([StatementField {
            value_type: StatementValueType::Int32,
            nullable: false,
        }]),
        // The claim path under test. A non-transactional statement with no
        // per-caller claim takes the autocommit branch instead.
        transactional: true,
    }
}

/// *** A COLD CONNECTION STILL PARSES INSIDE THE CLAIM TRANSACTION. ***
///
/// The regression guard for `wamn-0h0g.15.137.15` and the correctness half
/// of `wamn-0h0g.17.33`. The pool is brand new, so nothing on the physical
/// connection has been parsed; the statement names an unqualified relation
/// that exists ONLY in a schema outside the session's `search_path`. Parse
/// is where a relation name resolves, so this can succeed only if the
/// server ran the Parse after the transaction-LOCAL `search_path` the
/// claims install — that is, inside the claim transaction.
///
/// IT IS NOT RACY. On the pre-fix shape `begin_with_claims` opens by
/// awaiting its OWN `prepare_cached`, which on a cold connection is a
/// guaranteed round trip, and the statement half — polled during that await
/// — always sends its Parse first. Reverting to that shape, or swapping the
/// two branches of the `join!`, fails this every run.
#[tokio::test]
async fn live_a_cold_connection_parses_inside_the_claim_transaction() {
    const TENANT: &str = "coldparse";
    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let schema = format!("wamn_coldparse_{}", std::process::id());
    let role = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
    );
    let guest_url = live_guest_url(&admin_url, TENANT).await;
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&cold_parse_fixture_sql(&schema, &role))
        .await
        .expect("seed the cold-parse fixture as the superuser owner");

    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        // ONE connection, and a pool built in this test, so the checkout
        // below is guaranteed to be a NEW physical connection with an empty
        // statement cache.
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .expect("the plugin builds from the guest generation's url");
    let scope = "coldparse-instance-0";
    pg.bind_session_claims(
        scope,
        &SessionClaims {
            tenant: TENANT.to_string(),
            schema: Some(schema.clone()),
            ..SessionClaims::default()
        },
    )
    .expect("the cold-parse scope binds");

    let statement = cold_parse_statement();
    let rows = pg
        .one_shot_statement(scope, "sha256:cold-parse", &statement, &[])
        .await
        .expect(
            "a COLD connection must parse the statement inside the claim transaction: a \
                 `relation \"cold_parse\" does not exist` here is the statement reaching the \
                 server ahead of the claims",
        );
    assert_eq!(
        rows.rows.len(),
        2,
        "the claimed search_path resolved the relation"
    );

    // And the same connection, now WARM, still runs it: the flight that
    // saves the round trip is the one this second call takes.
    let again = pg
        .one_shot_statement(scope, "sha256:cold-parse", &statement, &[])
        .await
        .expect("the warm connection runs the pipelined flight");
    assert_eq!(again.rows.len(), 2);

    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
        ))
        .await
        .expect("drop the fixture");
}

/// The wamn-0h0g.17.33 measurement, runnable on demand.
///
/// OFF unless `WAMN_PG_PIPELINE_BENCH` is set, because it is a two-thousand
/// request loop, not an assertion. What it measures is ROUND TRIPS, and on a
/// loopback server one round trip is roughly 50 us -- under the noise of a
/// busy machine. Point `WAMN_PG_TEST_URL` at a server whose latency you can
/// see (a delaying TCP proxy in front of a container, or a real host) and
/// the count is legible: the claim transaction and the statement are ONE
/// flight, and `WAMN_PG_PIPELINE_BENCH_RUN` adds the run-owned causation
/// emit that rides the same BEGIN.
#[tokio::test]
async fn bench_pipelined_claim_flight() {
    const TENANT: &str = "pipebench";
    if std::env::var("WAMN_PG_PIPELINE_BENCH").is_err() {
        return;
    }
    let Some(admin_url) = test_pg_url() else {
        return;
    };
    let schema = format!("wamn_pipebench_{}", std::process::id());
    let role = format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
    );
    let guest_url = live_guest_url(&admin_url, TENANT).await;
    let admin = connect_raw(&admin_url).await;
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema}; \
                 CREATE TABLE {schema}.cold_parse (id int NOT NULL); \
                 INSERT INTO {schema}.cold_parse VALUES (1), (2); \
                 GRANT USAGE ON SCHEMA {schema} TO \"{role}\"; \
                 GRANT SELECT ON {schema}.cold_parse TO \"{role}\";"
        ))
        .await
        .unwrap();
    let pg = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    let scope = "pipebench-instance-0";
    let run = std::env::var("WAMN_PG_PIPELINE_BENCH_RUN").is_ok();
    pg.bind_session_claims(
        scope,
        &SessionClaims {
            tenant: TENANT.to_string(),
            schema: Some(schema.clone()),
            ..SessionClaims::default()
        },
    )
    .unwrap();
    if run {
        pg.set_current_run(
            scope,
            Some(Causation {
                run: "bench-run".to_string(),
                root: "bench-run".to_string(),
                depth: 0,
            }),
        );
    }
    let statement = VerifiedStatement {
        exact_sql: "SELECT id FROM cold_parse ORDER BY id".into(),
        binds: Box::new([]),
        columns: Box::new([StatementField {
            value_type: StatementValueType::Int32,
            nullable: false,
        }]),
        transactional: true,
    };
    for _ in 0..200 {
        pg.one_shot_statement(scope, "sha256:bench", &statement, &[])
            .await
            .unwrap();
    }
    let iterations: u32 = 2_000;
    let mut samples = Vec::with_capacity(iterations as usize);
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let one = std::time::Instant::now();
        pg.one_shot_statement(scope, "sha256:bench", &statement, &[])
            .await
            .unwrap();
        samples.push(one.elapsed().as_secs_f64() * 1_000.0);
    }
    let total = start.elapsed().as_secs_f64() * 1_000.0;
    samples.sort_by(f64::total_cmp);
    let pct = |percent: usize| samples[(samples.len() - 1) * percent / 100];
    println!(
        "BENCH run={run} n={iterations} total={total:.1}ms mean={:.4}ms \
             p50={:.4}ms p90={:.4}ms p99={:.4}ms",
        total / f64::from(iterations),
        pct(50),
        pct(90),
        pct(99),
    );
    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
        ))
        .await
        .unwrap();
}
