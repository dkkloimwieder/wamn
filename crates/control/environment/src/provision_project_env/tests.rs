use super::grants::plan::{
    RoleAcl, StableGrantSet, stable_grant_set, verify_audit_retention_grants,
    verify_connection_reader_grants, verify_event_materializer_grants, verify_http_admitter_grants,
    verify_session_role_reader_grants, verify_system_reader_grants,
};
use super::registry::{INSTANCE_SUFFIX_ALPHABET, do_record_project_env};
use super::workload::{
    WorkloadActionIdentity, WorkloadLifecycle, is_workload_generation_role, workload_lifecycle,
};
use super::*;

/// The mint is the whole non-reuse mechanism (wamn-0h0g.13.57): every draw
/// must satisfy the pure crate's rule, and the draws must actually differ.
/// A constant, a counter, or a triple-derived suffix collapses `seen`.
#[test]
fn the_minted_instance_suffix_is_a_fresh_valid_dns_label_tail() {
    // A duplicated symbol would bias the `% len()` fold; a symbol outside
    // `[a-z0-9]` would leave the namespace an illegal DNS-1123 label.
    assert_eq!(INSTANCE_SUFFIX_ALPHABET.len(), 36);
    assert_eq!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        INSTANCE_SUFFIX_ALPHABET.len()
    );
    assert!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    );

    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let suffix = mint_instance_suffix().expect("mint an instance suffix");
        validate_instance_suffix(&suffix).expect("a minted suffix satisfies the pure rule");
        seen.insert(suffix);
    }
    assert_eq!(seen.len(), 64, "64 draws collapsed to {}", seen.len());
}

#[test]
fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
    // The name validation runs first — a reserved / non-slug project id fails
    // without touching the registry or emitting a CR.
    assert!(validate_project_env("demo", "wamn-x", "dev").is_err());
    assert!(validate_project_env("demo", "Bad", "prod").is_err());
    assert!(validate_project_env("demo", "billing", "prod").is_ok());
}

/// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
/// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
/// collapses every env onto its pool. (The live routing through the DB is
/// checked by the in-cluster acceptance test; here we pin the pure derivation the
/// subcommand calls.)
#[test]
fn cluster_is_derived_by_placement_and_policy() {
    use wamn_control_registry::EnvPolicy;
    let ded = Org::dedicated("demo");
    assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "demo-dev");
    assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "demo-prod");
    let pooled = Org::pooled("try", "wamn-pg");
    assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
}

#[test]
fn pat_literals_and_secret_documents_are_exact() {
    let triple = Triple::new("demo", "billing", "dev");
    assert_eq!(PAT_TTL, Duration::from_hours(720));
    assert_eq!(
        MANAGEMENT_AUTHOR.subject(&triple).unwrap(),
        "wamn-management-author-demo--billing--dev"
    );
    assert_eq!(
        MANAGEMENT_AUTHOR.display_name(&triple),
        "WAMN management author demo/billing/dev"
    );
    assert_eq!(
        OPERATOR.subject(&triple).unwrap(),
        "wamn-operator-demo--billing--dev"
    );
    assert_eq!(
        OPERATOR.display_name(&triple),
        "WAMN operator demo/billing/dev"
    );

    let secret = render_pat_secret(
        &triple,
        "wamn-system",
        MANAGEMENT_AUTHOR,
        "6d3f2d1c-0000-4000-8000-00000000abcd",
        "wamn_pat_token-material",
        "0123456789abcdef",
        "2026-09-09T12:34:56Z",
    )
    .unwrap();
    assert_eq!(
        secret,
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": "wamn-pat-management-author-demo--billing--dev",
                "namespace": "wamn-system",
                "labels": {
                    "app.kubernetes.io/managed-by": "wamn",
                    "app.kubernetes.io/component": "project-env-pat",
                    "wamn.org": "demo",
                    "wamn.project": "billing",
                    "wamn.env": "dev",
                },
                "annotations": {
                    "wamn.io/credential-purpose": "management-author",
                    "wamn.io/principal-id": "6d3f2d1c-0000-4000-8000-00000000abcd",
                    "wamn.io/principal-type": "service",
                    "wamn.io/principal-subject": "wamn-management-author-demo--billing--dev",
                    "wamn.io/project-role": "project-author",
                    "wamn.io/pat-prefix": "0123456789abcdef",
                    "wamn.io/pat-expires-at": "2026-09-09T12:34:56Z",
                },
            },
            "type": "Opaque",
            "stringData": {
                "token": "wamn_pat_token-material",
            },
        })
    );
    assert_eq!(
        secret["stringData"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["token"]
    );

    let operator_secret = render_pat_secret(
        &triple,
        "wamn-system",
        OPERATOR,
        "6d3f2d1c-0000-4000-8000-00000000abcd",
        "wamn_pat_other-material",
        "fedcba9876543210",
        "2026-09-09T12:34:56Z",
    )
    .unwrap();
    assert_eq!(
        operator_secret["metadata"]["name"],
        "wamn-pat-operator-demo--billing--dev"
    );
    assert_eq!(
        operator_secret["metadata"]["annotations"]["wamn.io/project-role"],
        "admin"
    );
}

#[test]
fn secret_files_are_plain_json_and_mode_0600() {
    let path = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-mode-{}.json",
        std::process::id()
    ));
    std::fs::write(&path, b"old credential material").unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

    let document = json!({"stringData": {"token": "test-token"}});
    write_secret_json(&path, &document).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, document);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn secret_writes_replace_links_without_following_or_sharing_them() {
    let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-links-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();

    let db_path = root.join("db.json");
    let management_path = root.join("management.json");
    std::os::unix::fs::symlink(&db_path, &management_path).unwrap();
    let db_document = json!({"stringData": {"url": "db-secret"}});
    let management_document = json!({"stringData": {"token": "management-secret"}});
    write_secret_json(&db_path, &db_document).unwrap();
    write_secret_json(&management_path, &management_document).unwrap();
    assert!(
        !std::fs::symlink_metadata(&management_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let stored_db: Value = serde_json::from_slice(&std::fs::read(&db_path).unwrap()).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    assert_eq!(stored_db, db_document);
    assert_eq!(stored_management, management_document);

    let route_path = root.join("route.json");
    std::fs::hard_link(&management_path, &route_path).unwrap();
    let route_document = json!({"stringData": {"token": "route-secret"}});
    write_secret_json(&route_path, &route_document).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    let stored_route: Value = serde_json::from_slice(&std::fs::read(&route_path).unwrap()).unwrap();
    assert_eq!(stored_management, management_document);
    assert_eq!(stored_route, route_document);

    std::fs::remove_file(db_path).unwrap();
    std::fs::remove_file(management_path).unwrap();
    std::fs::remove_file(route_path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

/// wamn-0h0g.12.122. The emitted privilege batch is PINNED whole: a runtime
/// condition that only asserts "the reader can connect" stays green when a
/// builder is swapped for a wider one, and stays green when a `CONNECT`
/// statement drifts back above the owner statement on a database that
/// happens to be owned by `wamn_db_owner` already. The frozen literal is the
/// guard.
///
/// wamn-0h0g.12.179 re-pinned it once, moving `wamn_app` from granted to
/// revoked. The batch grants `CONNECT` to nobody. Every principal that reaches a project-env
/// database is a generation, and a generation is granted `CONNECT` directly
/// by its own prepare.
#[test]
fn the_privilege_batch_revokes_every_stable_role_connect_and_sets_no_owner() {
    let batch = privilege_sql("wamn-db-demo--billing--dev");
    assert_eq!(
        batch,
        "REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-demo--billing--dev\" FROM PUBLIC; \
             REVOKE CONNECT ON DATABASE \"wamn-db-demo--billing--dev\" FROM \"wamn_app\";\n"
    );

    // NOBODY is granted CONNECT here, and the PUBLIC confinement stands.
    assert!(!batch.contains("GRANT CONNECT"));
    assert!(batch.contains("REVOKE CONNECT, TEMPORARY ON DATABASE"));
}

/// wamn-0h0g.12.179. The stable guest ACL role must never be handed
/// `CONNECT` by the batch an operator applies. It is cluster-global and
/// every per-project generation INHERITS it, so one grant here reaches every
/// project-env database on the cluster — and `--prepare-guest-generation`
/// refuses the result, which is how the defect surfaced.
#[test]
fn the_privilege_batch_never_grants_the_stable_guest_acl_role_connect() {
    let batch = privilege_sql("wamn-db-demo--billing--dev");
    assert!(
        !batch.contains(&format!("TO \"{APP_ROLE}\"")),
        "the batch must not grant the stable guest ACL role anything: {batch}"
    );
    assert!(
        batch.contains(&format!(
            "REVOKE CONNECT ON DATABASE \"wamn-db-demo--billing--dev\" FROM \"{APP_ROLE}\""
        )),
        "the batch must CONVERGE a pre-cutover CONNECT away: {batch}"
    );
}

/// The role batch must create both stable principals as NOLOGIN carriers.
///
/// Before wamn-0h0g.12.122 the example manifest named a dispatch role
/// production provisioning never created; wamn-0h0g.22.24 keeps it created
/// without a credential. `wamn-0h0g.12.140` gives `wamn_app` the same
/// posture, and `wamn-xv69` deleted the legacy password argument outright.
#[test]
fn the_role_batch_creates_passwordless_nologin_acl_roles() {
    let batch = role_sql();
    assert_eq!(
        role_posture_sql(),
        format!(
            "{app}\n{owner}\n",
            app = sql::ensure_app_acl_role_sql(),
            owner = sql::ensure_db_owner_role_sql(),
        )
    );
    assert_eq!(
        batch,
        format!(
            "{posture}\n{drain}\n",
            posture = role_posture_sql(),
            drain = sql::drain_app_role_sessions_sql(),
        )
    );
    assert!(batch.contains("'wamn_app'"));
    assert!(batch.contains("CREATE ROLE \"wamn_app\" NOLOGIN"));
    assert!(batch.contains("ALTER ROLE \"wamn_app\" NOLOGIN PASSWORD NULL"));
    assert!(batch.contains("CREATE ROLE wamn_db_owner NOLOGIN"));
    assert!(batch.contains("ALTER ROLE wamn_db_owner NOLOGIN"));
}

/// The dispatch, the lifecycle and the ACL expectation are one derivation
/// each, total over the family set.
///
/// Four copy-pasted `run_*_action` functions, four lifecycle constructors
/// and a two-arm `RoleAclExpectation` are gone. What stays per family is the
/// GRANT SET, and the wildcard arm of [`stable_grant_set`] means a family
/// admitted without authority needs no entry there either.
#[test]
fn every_family_derives_a_lifecycle_and_only_a_grant_set_stays_per_family() {
    let identity = WorkloadActionIdentity {
        org: "demo",
        project: "billing",
        environment: "dev",
    };
    for family in WorkloadRoleFamily::ALL {
        let lifecycle =
            workload_lifecycle(family, identity, "project", "wamn-db-demo--billing--dev");
        assert_eq!(lifecycle.family, family);
        assert_eq!(lifecycle.database(), "wamn-db-demo--billing--dev");
        // The scope grain is the family's own declaration, so a family can
        // never be paired with the wrong one here.
        assert_eq!(lifecycle.scope, {
            let probe =
                workload_lifecycle(family, identity, "project", "wamn-db-demo--billing--dev");
            probe.scope
        });
        let a = lifecycle.role(CredentialGeneration::A);
        let b = lifecycle.role(CredentialGeneration::B);
        assert_ne!(a, b, "{family:?}");
        assert!(is_workload_generation_role(family, &a), "{family:?}: {a}");
        assert!(a.len() <= 63, "{family:?}: {a}");
        // Only a CONTROL-scoped family records a login-to-project mapping.
        assert_eq!(
            lifecycle.control_project_env_key.is_some(),
            family.scope_kind() == WorkloadRoleScopeKind::Control,
            "{family:?}"
        );
    }

    let with_grant_sets: Vec<WorkloadRoleFamily> = WorkloadRoleFamily::ALL
        .into_iter()
        .filter(|family| stable_grant_set(*family).is_some())
        .collect();
    assert_eq!(
        with_grant_sets,
        [
            // `wamn-0h0g.12.69`: run retention acquired authority — DELETE
            // plus a three-column SELECT on `runs` — so it acquires a
            // denial matrix here at the same time. The two are the same
            // event, and a family with one and not the other is the bug
            // this assertion exists to catch.
            WorkloadRoleFamily::Retention,
            // `wamn-0h0g.22.37`: the executor-platform and callable-HTTP
            // families acquired the grant sets their MEASURED production
            // surfaces derive, so both acquire a denial matrix with them.
            WorkloadRoleFamily::ExecutorPlatform,
            WorkloadRoleFamily::HttpAdmitter,
            WorkloadRoleFamily::EventMaterializer,
            WorkloadRoleFamily::RegistryReader,
            WorkloadRoleFamily::IdentityReader,
            WorkloadRoleFamily::SessionRoleReader,
            // `wamn-emtx.13`: audit retention acquired DELETE and a
            // three-column SELECT on P<n>D history tables, with its denial
            // matrix row.
            WorkloadRoleFamily::AuditRetention,
            // `wamn-a40n.2`: the administration family reads the application
            // authorization relations, with its denial matrix row.
            WorkloadRoleFamily::Administration,
            // `wamn-a40n.2`: the control family reads the identity relations
            // of the control host.
            WorkloadRoleFamily::Control,
            WorkloadRoleFamily::ConnectionReader,
        ],
        "a family acquired a grant set without acquiring authority"
    );
    // The pre-prepare grant-set assertion fires for the families whose grant
    // set is converged ELSEWHERE, and not for the ones this batch applies.
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::Retention).is_none());
    // apply-package converges the audit retention grants, not this batch.
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::AuditRetention).is_none());
    for family in [
        // `wamn-0h0g.22.37`: this batch applies both new surfaces, so the
        // pre-prepare assertion must NOT fire for them — on a first prepare
        // there is nothing converged yet to assert against.
        WorkloadRoleFamily::ExecutorPlatform,
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleFamily::EventMaterializer,
        WorkloadRoleFamily::RegistryReader,
        WorkloadRoleFamily::IdentityReader,
        WorkloadRoleFamily::SessionRoleReader,
        WorkloadRoleFamily::Administration,
        WorkloadRoleFamily::Control,
        WorkloadRoleFamily::ConnectionReader,
    ] {
        assert!(sql::stable_surface_sql(family).is_some(), "{family:?}");
    }
    for family in WorkloadRoleFamily::ALL {
        if !matches!(
            family,
            WorkloadRoleFamily::ExecutorPlatform
                | WorkloadRoleFamily::HttpAdmitter
                | WorkloadRoleFamily::EventMaterializer
                | WorkloadRoleFamily::RegistryReader
                | WorkloadRoleFamily::IdentityReader
                | WorkloadRoleFamily::SessionRoleReader
                | WorkloadRoleFamily::Administration
                | WorkloadRoleFamily::Control
                | WorkloadRoleFamily::ConnectionReader
        ) {
            assert!(sql::stable_surface_sql(family).is_none(), "{family:?}");
        }
    }
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::EventMaterializer).is_some());
    assert_eq!(
        stable_grant_set(WorkloadRoleFamily::EventMaterializer),
        Some(StableGrantSet::EventMaterializer)
    );
}

/// THE DISJOINTNESS MATRIX, exercised from both sides
/// (`wamn-0h0g.12.116`).
///
/// The exact set passes; the same set widened onto the identity plane, or
/// widened with a write privilege, does not. The empty grant set is required
/// in the control database and required to be empty everywhere else.
#[test]
fn the_registry_reader_grants_are_exact_and_never_reach_identity() {
    let exact = vec![
        role_acl("schema", "registry", "registry", "USAGE"),
        role_acl("relation", "registry", "event_readers", "SELECT"),
        role_acl("relation", "registry", "capture_gap", "SELECT"),
    ];
    let verify = |grants: &[RoleAcl], database: &str| {
        verify_system_reader_grants(
            SystemReader::Registry,
            "registry",
            &sql::REGISTRY_READER_RELATIONS,
            &[],
            WorkloadRoleFamily::RegistryReader.acl_role(),
            database,
            "wamn_system",
            grants,
        )
    };
    verify(&exact, "wamn_system").unwrap();

    for widening in [
        role_acl("schema", "identity", "identity", "USAGE"),
        role_acl("relation", "identity", "pats", "SELECT"),
        role_acl("relation", "identity", "project_roles", "SELECT"),
        role_acl("relation", "registry", "event_readers", "INSERT"),
        role_acl("relation", "registry", "event_readers", "UPDATE"),
        role_acl("relation", "registry", "orgs", "SELECT"),
    ] {
        let mut widened = exact.clone();
        widened.push(widening.clone());
        let error = verify(&widened, "wamn_system")
            .expect_err("a widened registry reader passed its own matrix");
        assert!(
            error
                .to_string()
                .contains("are not the exact registry-reader grant set"),
            "refused for the wrong reason: {error}"
        );
    }

    // A missing grant set in the control database is a failure; nothing at
    // all in any OTHER database is the required state.
    verify(&[], "wamn_system").expect_err("an empty control-database grant set passed");
    verify(&[], "some_project_db").unwrap();
    let error = verify(&exact, "some_project_db")
        .expect_err("the reader holds its grant set in a database that is not the control one");
    assert!(
        error
            .to_string()
            .contains("which is not the control database"),
        "refused for the wrong reason: {error}"
    );
}

/// THE DISJOINTNESS MATRIX from the identity side, and THE THREE-TIMES-DRIFT
/// GUARD at the verification boundary (`wamn-0h0g.12.67`).
///
/// The live cluster role carries `SELECT, INSERT, UPDATE`. Every one of the
/// widenings below — the registry plane, `INSERT`, `UPDATE` — is measured
/// against the server's own `aclexplode` answer for EQUALITY, so a role that
/// drifts a fourth time fails provisioning instead of being converged around.
#[test]
fn the_identity_reader_grants_are_exact_and_never_allow_a_write() {
    let exact = vec![
        role_acl("schema", "identity", "identity", "USAGE"),
        role_acl("relation", "identity", "pats", "SELECT"),
        role_acl("relation", "identity", "password_logins", "SELECT"),
        role_acl("relation", "identity", "principals", "SELECT"),
        role_acl("relation", "identity", "project_env_memberships", "SELECT"),
        role_acl("relation", "identity", "project_roles", "SELECT"),
        role_acl(
            "column",
            "identity",
            "org_memberships.principal_id",
            "SELECT",
        ),
        role_acl("column", "identity", "org_memberships.org", "SELECT"),
        role_acl("column", "identity", "org_memberships.status", "SELECT"),
    ];
    let verify = |grants: &[RoleAcl], database: &str| {
        verify_system_reader_grants(
            SystemReader::Identity,
            "identity",
            &sql::IDENTITY_READER_RELATIONS,
            &sql::IDENTITY_READER_COLUMNS,
            WorkloadRoleFamily::IdentityReader.acl_role(),
            database,
            "wamn_system",
            grants,
        )
    };
    verify(&exact, "wamn_system").unwrap();

    for widening in [
        // The forgery primitives themselves.
        role_acl("relation", "identity", "pats", "INSERT"),
        role_acl("relation", "identity", "pats", "UPDATE"),
        role_acl("relation", "identity", "project_roles", "INSERT"),
        role_acl("relation", "identity", "project_roles", "UPDATE"),
        role_acl("relation", "identity", "project_env_memberships", "INSERT"),
        role_acl("relation", "identity", "project_env_memberships", "UPDATE"),
        role_acl("relation", "identity", "project_env_memberships", "DELETE"),
        role_acl("column", "identity", "pats.token_hash", "UPDATE"),
        role_acl("column", "identity", "org_memberships.status", "UPDATE"),
        role_acl("relation", "identity", "org_memberships", "SELECT"),
        // …and the other reader's plane.
        role_acl("schema", "registry", "registry", "USAGE"),
        role_acl("relation", "registry", "event_readers", "SELECT"),
    ] {
        let mut widened = exact.clone();
        widened.push(widening.clone());
        let error = verify(&widened, "wamn_system")
            .expect_err("a widened identity reader passed its own matrix");
        assert!(
            error
                .to_string()
                .contains("are not the exact identity-reader grant set"),
            "refused for the wrong reason: {error}"
        );
    }
    // The membership columns are part of the exact set, so a reader without
    // one of them fails too.
    let narrowed = exact
        .iter()
        .filter(|acl| acl.object_name != "org_memberships.status")
        .cloned()
        .collect::<Vec<_>>();
    verify(&narrowed, "wamn_system").expect_err("a reader without a membership column passed");
    verify(&[], "wamn_system").expect_err("an empty control-database grant set passed");
    verify(&[], "some_project_db").unwrap();
}

#[test]
fn the_http_admitter_grants_require_fresh_reads_and_refuse_writes() {
    let exact = sql::HTTP_ADMITTER_SCHEMAS
        .iter()
        .map(|schema| role_acl("schema", schema, schema, "USAGE"))
        .chain(
            sql::http_admitter_relations()
                .map(|(schema, relation)| role_acl("relation", schema, relation, "SELECT")),
        )
        .chain(sql::HTTP_ADMITTER_WORKFLOW_COLUMNS.iter().map(|column| {
            role_acl(
                "column",
                "catalog",
                &format!("workflows.{column}"),
                "SELECT",
            )
        }))
        .collect::<Vec<_>>();
    assert!(
        exact.iter().all(|acl| acl.schema_name != "wamn_cache"),
        "WAL-based ETags need no counter schema grant"
    );
    let verify = |grants: &[RoleAcl]| {
        verify_http_admitter_grants(
            WorkloadRoleFamily::HttpAdmitter.acl_role(),
            "project_db",
            "project_db",
            grants,
        )
    };
    verify(&exact).expect("the fresh permission reads have their exact grant set");
    for relation in ["user", "user_role", "permission"] {
        let mut missing = exact.clone();
        missing.retain(|acl| acl.schema_name != "platform" || acl.object_name != relation);
        verify(&missing).expect_err("a required fresh permission read is missing");
        for privilege in ["INSERT", "UPDATE", "DELETE"] {
            let mut widened = exact.clone();
            widened.push(role_acl("relation", "platform", relation, privilege));
            verify(&widened).expect_err("the HTTP admitter cannot write permission authority");
        }
    }
    let mut missing = exact.clone();
    missing.retain(|acl| acl.object_name != "workflows.graph_json");
    verify(&missing).expect_err("the workflow graph read is missing");
    let mut widened = exact;
    widened.push(role_acl("relation", "catalog", "workflows", "SELECT"));
    verify(&widened).expect_err("the HTTP admitter reads the whole workflows table");
}

/// Every family publishes a Secret whose name, component label and body are
/// DERIVED, and the three frozen names are unchanged.
#[test]
fn every_family_derives_its_credential_secret_name() {
    let frozen = [(WorkloadRoleFamily::App, "wamn-guest-demo--billing--dev")];
    {
        let [(family, name)] = frozen;
        assert_eq!(
            wamn_control_provision::workload_secret_name(family, "demo", "billing", "dev"),
            name,
            "{family:?}"
        );
    }
    let mut names = BTreeSet::new();
    for family in WorkloadRoleFamily::ALL {
        let name = wamn_control_provision::workload_secret_name(family, "demo", "billing", "dev");
        assert!(name.starts_with("wamn-"), "{family:?}: {name}");
        assert!(names.insert(name), "{family:?} shares a Secret name");
    }
    assert_eq!(names.len(), WorkloadRoleFamily::ALL.len());
}

fn role_acl(kind: &str, schema: &str, object: &str, privilege: &str) -> RoleAcl {
    RoleAcl {
        object_type: kind.to_string(),
        schema_name: schema.to_string(),
        object_name: object.to_string(),
        privilege: privilege.to_string(),
        grantable: false,
    }
}

#[test]
fn connection_reader_requires_exact_grants_with_the_oauth_rows() {
    let mut exact = vec![
        role_acl("schema", "registry", "registry", "USAGE"),
        role_acl("schema", "identity", "identity", "USAGE"),
    ];
    for table in ["oauth_token", "oauth_authorization"] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            exact.push(role_acl("relation", "identity", table, privilege));
        }
    }
    for routine in [
        "lock_oauth_connection",
        "read_file_connection",
        "read_connection_fact",
        "read_connection_generation",
        "read_connection_secret",
        "read_env_connections",
        "read_integration_version",
    ] {
        exact.push(role_acl("routine", "registry", routine, "EXECUTE"));
    }
    let verify = |rows: &[RoleAcl]| {
        verify_connection_reader_grants("wamn_conn_reader", "wamn_system", "wamn_system", rows)
    };
    assert!(verify(&exact).is_ok());
    for index in 0..exact.len() {
        let mut missing = exact.clone();
        missing.remove(index);
        assert!(verify(&missing).is_err());
    }
    let mut widened = exact.clone();
    widened.push(role_acl("relation", "identity", "principals", "SELECT"));
    assert!(verify(&widened).is_err());
    assert!(
        verify_connection_reader_grants("wamn_conn_reader", "other-db", "wamn_system", &[]).is_ok()
    );
}

#[test]
fn session_reader_requires_exact_grants() {
    let mut exact = vec![role_acl("schema", "platform", "platform", "USAGE")];
    for column in [
        "user.id",
        "user.status",
        "user_role.user_id",
        "user_role.role_name",
    ] {
        exact.push(role_acl("column", "platform", column, "SELECT"));
    }
    let verify = |rows: &[RoleAcl]| {
        verify_session_role_reader_grants(
            "wamn_session_role_reader",
            "project-db",
            "project-db",
            rows,
        )
    };
    assert!(verify(&exact).is_ok());
    for index in 0..exact.len() {
        let mut missing = exact.clone();
        missing.remove(index);
        assert!(verify(&missing).is_err());
    }
    for extra in [
        role_acl("column", "platform", "user_role.project_env_key", "SELECT"),
        role_acl("relation", "platform", "user", "SELECT"),
        role_acl("column", "platform", "user.email", "SELECT"),
        role_acl("column", "platform", "user.status", "UPDATE"),
        role_acl("relation", "platform", "permission", "SELECT"),
        role_acl("schema", "catalog", "catalog", "USAGE"),
    ] {
        let mut widened = exact.clone();
        widened.push(extra);
        assert!(verify(&widened).is_err());
    }
    let mut grantable = exact.clone();
    grantable[0].grantable = true;
    assert!(verify(&grantable).is_err());
    assert!(
        verify_session_role_reader_grants(
            "wamn_session_role_reader",
            "other-db",
            "project-db",
            &[],
        )
        .is_ok()
    );
}

/// The audit retention grants follow the P<n>D history tables exactly.
#[test]
fn audit_retention_requires_exact_grants_on_the_retention_targets() {
    let targets = vec![
        ("orders".to_string(), "widget_history".to_string()),
        ("orders".to_string(), "widget_tag_history".to_string()),
    ];
    let mut exact = vec![role_acl("schema", "orders", "orders", "USAGE")];
    for history in ["widget_history", "widget_tag_history"] {
        exact.push(role_acl("relation", "orders", history, "DELETE"));
        for column in ["row_key", "position", "changed_at", "transaction_id"] {
            exact.push(role_acl(
                "column",
                "orders",
                &format!("{history}.{column}"),
                "SELECT",
            ));
        }
    }
    exact.push(role_acl("schema", "wamn_history", "wamn_history", "USAGE"));
    for privilege in ["SELECT", "INSERT", "UPDATE"] {
        exact.push(role_acl(
            "relation",
            "wamn_history",
            "pruning_watermarks",
            privilege,
        ));
    }
    let verify = |rows: &[RoleAcl], targets: &[(String, String)]| {
        verify_audit_retention_grants("wamn_audit_retention", "project-db", rows, targets)
    };
    assert!(verify(&exact, &targets).is_ok());
    assert!(verify(&[], &[]).is_ok());
    let write_log = vec![
        role_acl("schema", "platform", "platform", "USAGE"),
        role_acl("relation", "platform", "write_log", "DELETE"),
        role_acl("column", "platform", "write_log.created_at", "SELECT"),
        role_acl("column", "platform", "write_log.result", "SELECT"),
    ];
    let write_log_target = ("platform".to_string(), "write_log".to_string());
    assert!(verify(&write_log, std::slice::from_ref(&write_log_target)).is_ok());
    let mut combined = exact.clone();
    combined.extend(write_log);
    let mut combined_targets = targets.clone();
    combined_targets.push(write_log_target);
    assert!(verify(&combined, &combined_targets).is_ok());
    combined.push(role_acl(
        "column",
        "platform",
        "write_log.request",
        "SELECT",
    ));
    assert!(verify(&combined, &combined_targets).is_err());
    for index in 0..exact.len() {
        let mut missing = exact.clone();
        missing.remove(index);
        assert!(verify(&missing, &targets).is_err());
    }
    for extra in [
        role_acl("relation", "orders", "widget_history", "SELECT"),
        role_acl("column", "orders", "widget_history.before", "SELECT"),
        role_acl("relation", "orders", "widget_history", "INSERT"),
        role_acl("relation", "orders", "widget", "DELETE"),
        // The history table of an unlimited relation is not a target.
        role_acl("relation", "orders", "ledger_history", "DELETE"),
        role_acl("schema", "platform", "platform", "USAGE"),
        role_acl("routine", "wamn_history", "log_row_change", "EXECUTE"),
    ] {
        let mut widened = exact.clone();
        widened.push(extra);
        assert!(verify(&widened, &targets).is_err());
    }
    // A target in a reserved schema refuses, even with its exact grants.
    let reserved = vec![("platform".to_string(), "user_history".to_string())];
    let mut platform = vec![role_acl("schema", "platform", "platform", "USAGE")];
    platform.push(role_acl("relation", "platform", "user_history", "DELETE"));
    for column in ["row_key", "position", "changed_at", "transaction_id"] {
        platform.push(role_acl(
            "column",
            "platform",
            &format!("user_history.{column}"),
            "SELECT",
        ));
    }
    assert!(verify(&platform, &reserved).is_err());
}

#[test]
fn materializer_grants_cover_exactly_the_two_production_reads() {
    let mut exact = vec![role_acl("schema", "catalog", "catalog", "USAGE")];
    for relation in sql::EVENT_MATERIALIZER_CATALOG_RELATIONS {
        exact.push(role_acl("relation", "catalog", relation, "SELECT"));
    }
    assert!(
        verify_event_materializer_grants("wamn_event_materializer", "wamn", "wamn", &exact,)
            .is_ok()
    );

    let mut widened = exact.clone();
    widened.push(role_acl("relation", "catalog", "packages", "INSERT"));
    assert!(
        verify_event_materializer_grants("wamn_event_materializer", "wamn", "wamn", &widened,)
            .is_err()
    );
    assert!(
        verify_event_materializer_grants("wamn_event_materializer", "wamn", "wamn", &[],).is_err()
    );
}

#[test]
fn every_workload_family_carries_a_distinct_frozen_label() {
    // `wamn-0fqa` takes the vocabulary to ten and `wamn-0h0g.13.63` to
    // twelve. `wamn-ctc8.15.2` adds the session-role reader as the thirteenth,
    // and `wamn-emtx.13` adds audit retention as the fourteenth.
    // `wamn-0h0g.10.15` removes the effect writer, leaving thirteen.
    // `label` reads only the family, so the scope is deliberately uniform.
    let expected = [
        (WorkloadRoleFamily::ServiceReader, "service-reader"),
        (WorkloadRoleFamily::App, "app"),
        (WorkloadRoleFamily::Retention, "retention"),
        (WorkloadRoleFamily::ExecutorPlatform, "executor-platform"),
        (WorkloadRoleFamily::HttpAdmitter, "http-admitter"),
        (WorkloadRoleFamily::EventMaterializer, "event-materializer"),
        (WorkloadRoleFamily::RegistryReader, "registry-reader"),
        (WorkloadRoleFamily::IdentityReader, "identity-reader"),
        (WorkloadRoleFamily::SessionRoleReader, "session-role-reader"),
        (WorkloadRoleFamily::AuditRetention, "audit-retention"),
        (WorkloadRoleFamily::Administration, "administration"),
        (WorkloadRoleFamily::Control, "control"),
        (WorkloadRoleFamily::ConnectionReader, "conn-reader"),
    ];
    assert_eq!(expected.len(), WorkloadRoleFamily::ALL.len());
    let mut seen = Vec::new();
    for (family, label) in expected {
        let lifecycle = WorkloadLifecycle {
            family,
            scope: WorkloadRoleScope::ProjectEnvKey {
                project_env_key: "t",
                database: "db",
            },
            control_project_env_key: None,
        };
        assert_eq!(lifecycle.label(), label, "{family:?}");
        assert_eq!(family.label(), label, "{family:?}");
        seen.push(label);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), expected.len(), "labels must stay distinct");
}

#[test]
fn stable_acl_role_members_are_only_scoped_generation_roles() {
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::RegistryReader,
        "wamn_registry_reader_0123456789abcdef0123456789abcdef01234567_b"
    ));
    // `wamn-0fqa`: the executor-platform and event-materializer families
    // carry short frozen prefixes for the same reason; the callable-HTTP
    // admitter's name fits and keeps its ACL role name as the prefix.
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::ExecutorPlatform,
        "wamn_exec_platform_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(!is_workload_generation_role(
        WorkloadRoleFamily::ExecutorPlatform,
        "wamn_executor_platform_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::EventMaterializer,
        "wamn_materializer_0123456789abcdef0123456789abcdef01234567_b"
    ));
    assert!(!is_workload_generation_role(
        WorkloadRoleFamily::EventMaterializer,
        "wamn_event_materializer_0123456789abcdef0123456789abcdef01234567_b"
    ));
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        "wamn_http_admitter_0123456789abcdef0123456789abcdef01234567_a"
    ));
    for invalid in [
        "wamn_registry_reader_a",
        "wamn_registry_reader_0123456789ABCDEF0123456789abcdef01234567_a",
        "wamn_registry_reader_0123456789abcdef0123456789abcdef01234567_c",
        "unrelated_0123456789abcdef0123456789abcdef01234567_a",
    ] {
        assert!(
            !is_workload_generation_role(WorkloadRoleFamily::RegistryReader, invalid),
            "accepted {invalid}"
        );
    }
}

#[tokio::test]
async fn project_env_key_projection_and_instance_claim_hold_on_postgres() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let url = database.url();
    let connect = async || {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
        tokio::spawn(async move {
            connection.await.unwrap();
        });
        client
    };
    let mut first = connect().await;
    first.batch_execute("SET ROLE wamn_system").await.unwrap();
    first.batch_execute("BEGIN; SET LOCAL app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs (id,placement_type) VALUES ('demo','dedicated'); INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('demo','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18'); COMMIT;").await.unwrap();
    let triple = Triple::new("demo", "inventory", "dev");
    let other = Triple::new("demo", "shipping", "dev");
    let mut second = connect().await;
    second.batch_execute("SET ROLE wamn_system").await.unwrap();
    let (one, two) = tokio::join!(
        project_env_key_environment(&mut first, &triple, Some("project-race"), "abcd1234", false),
        project_env_key_environment(&mut second, &other, Some("project-race"), "efgh5678", true),
    );
    assert_ne!(
        one.is_ok(),
        two.is_ok(),
        "only one first project identity wins"
    );
    let (winner, suffix, disposable, error) = match (one, two) {
        (Ok(()), Err(error)) => (&triple, "abcd1234", false, error),
        (Err(error), Ok(())) => (&other, "efgh5678", true, error),
        results => panic!("unexpected project projection results: {results:?}"),
    };
    assert!(matches!(
        error.downcast_ref::<wamn_control_provision::ProvisionError>(),
        Some(
            wamn_control_provision::ProvisionError::ProjectEnvKeyEnvironmentIdentityConflict { .. }
        )
    ));
    assert!(
        error
            .to_string()
            .starts_with("project-environment-identity-projection-content-conflict:")
    );
    first
        .batch_execute("SET app.project_env_key='project-race'")
        .await
        .unwrap();
    let row = first.query_one("SELECT org,project,env,instance_suffix,disposable,environment_instance FROM catalog.project_environments WHERE project_env_key='project-race'", &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), winner.org);
    assert_eq!(row.get::<_, String>(1), winner.project);
    assert_eq!(row.get::<_, String>(2), winner.env.as_str());
    assert_eq!(row.get::<_, String>(3), suffix);
    assert_eq!(row.get::<_, bool>(4), disposable);
    assert_eq!(row.get::<_, String>(5), "");
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    wamn_test_infrastructure::record_history::assert_stamp(
        &first,
        "catalog.project_environments",
        "project_env_key='project-race'",
        &actor,
        &actor,
    )
    .await;

    {
        // The losing first insert must compare the committed winner's full identity.
        let mut held = connect().await;
        let transaction = held.transaction().await.unwrap();
        transaction
            .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await
            .unwrap();
        transaction
            .execute(
                sql::insert_project_env_key_environment_sql(),
                &[
                    &"project-held",
                    &triple.org,
                    &triple.project,
                    &triple.env.as_str(),
                    &"abcd1234",
                    &false,
                ],
            )
            .await
            .unwrap();
        let waiting = project_env_key_environment(
            &mut second,
            &other,
            Some("project-held"),
            "efgh5678",
            true,
        );
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut waiting)
                .await
                .is_err()
        );
        transaction.commit().await.unwrap();
        let error = waiting.await.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("project-environment-identity-projection-content-conflict:")
        );
    }
    do_record_project_env(
        &mut first,
        &triple,
        Some("org-a"),
        None,
        "cloud",
        "db-first",
        Some("system"),
        "abcd1234",
        Some(false),
        &wamn_broker::Publisher::default(),
    )
    .await
    .unwrap();
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    for relation in ["registry.projects", "registry.project_envs"] {
        wamn_test_infrastructure::record_history::assert_stamp(
            &first,
            relation,
            "org = 'demo'",
            &actor,
            &actor,
        )
        .await;
    }
    claim_environment_instance(&mut connect().await, "org-a", "16384")
        .await
        .unwrap();
    first
        .batch_execute("SET app.project_env_key='org-a'")
        .await
        .unwrap();
    let before: chrono::DateTime<chrono::Utc> = first
        .query_one(
            "SELECT projected_at FROM catalog.project_environments WHERE project_env_key='org-a'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let stored = do_record_project_env(
        &mut first,
        &triple,
        Some("org-a"),
        None,
        "cloud",
        "db-second",
        Some("system"),
        "efgh5678",
        Some(true),
        &wamn_broker::Publisher::default(),
    )
    .await
    .unwrap();
    assert_eq!(stored, "abcd1234", "the registry keeps its original suffix");
    let row = first.query_one("SELECT environment_instance,projected_at,instance_suffix,disposable FROM catalog.project_environments WHERE project_env_key='org-a'", &[]).await.unwrap();
    assert_eq!(
        row.get::<_, String>(0),
        "",
        "same-suffix projection clears the instance"
    );
    assert!(row.get::<_, chrono::DateTime<chrono::Utc>>(1) > before);
    assert_eq!(row.get::<_, String>(2), "abcd1234");
    assert!(row.get::<_, bool>(3));
    let stable_at: chrono::DateTime<chrono::Utc> = row.get(1);
    let error = do_record_project_env(
        &mut first,
        &other,
        Some("org-a"),
        None,
        "cloud",
        "db-other",
        Some("system"),
        "efgh5678",
        Some(true),
        &wamn_broker::Publisher::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("project-environment-identity-projection-content-conflict:")
    );
    let error = do_record_project_env(
        &mut first,
        &other,
        Some("org-b"),
        None,
        "nowhere",
        "db-other",
        Some("system"),
        "efgh5678",
        Some(true),
        &wamn_broker::Publisher::default(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("target \"nowhere\" is not in registry.target"),
        "{error:#}"
    );
    let row = first
        .query_one(
            "SELECT project,projected_at FROM catalog.project_environments WHERE project_env_key='org-a'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "inventory");
    assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>(1), stable_at);
    assert_eq!(first.query_one("SELECT secret_name FROM registry.project_envs WHERE org='demo' AND project='shipping' AND env='dev'", &[]).await.unwrap().get::<_, String>(0), "db-other", "the earlier registry commit survives projection refusal");

    claim_environment_instance(&mut connect().await, "org-a", "")
        .await
        .unwrap();
    let absent = claim_environment_instance(&mut connect().await, "project-unprojected", "16384")
        .await
        .unwrap_err();
    assert!(
        absent
            .to_string()
            .contains("environment-instance-claim-without-projection")
    );
    assert!(
        absent
            .to_string()
            .contains("name the project when provisioning the project-env")
    );
    assert_eq!(first.query_one("SELECT count(*) FROM catalog.project_environments WHERE project_env_key='project-unprojected'", &[]).await.unwrap().get::<_, i64>(0), 0);
    project_env_key_environment(&mut second, &other, Some("org-b"), "efgh5678", false)
        .await
        .unwrap();
    assert_eq!(
        first
            .query_one(
                "SELECT count(*) FROM catalog.project_environments WHERE project_env_key='org-b'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0,
        "the claimed project cannot read another project"
    );
    let foreign = first
        .execute(
            wamn_schema_control::claim_environment_instance_sql(),
            &[&"org-b", &"foreign"],
        )
        .await
        .unwrap();
    assert_eq!(
        foreign, 0,
        "the claimed project cannot change another project"
    );

    let lock = first.transaction().await.unwrap();
    lock.query_one(sql::read_project_env_key_environment_sql(), &[&"org-a"])
        .await
        .unwrap();
    let refresh =
        project_env_key_environment(&mut second, &triple, Some("org-a"), "1234abcd", false);
    tokio::pin!(refresh);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut refresh)
            .await
            .is_err(),
        "projection must wait for the project row lock"
    );
    lock.rollback().await.unwrap();
    refresh.await.unwrap();
    assert_eq!(
        first
            .query_one(
                "SELECT instance_suffix FROM catalog.project_environments WHERE project_env_key='org-a'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        "1234abcd"
    );
    first.batch_execute("RESET ROLE; DROP SCHEMA catalog CASCADE; DROP SCHEMA wamn_run CASCADE; DROP SCHEMA wamn_authority CASCADE; DROP SCHEMA registry CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA identity CASCADE").await.unwrap();
}

#[test]
fn workload_urls_use_the_database_service_namespace_unless_overridden() {
    for (explicit, cluster, namespace, expected) in [
        (
            None,
            "wamn-pg",
            "platform",
            "wamn-pg-rw.platform.svc.cluster.local",
        ),
        (
            None,
            "project-pg",
            "databases",
            "project-pg-rw.databases.svc.cluster.local",
        ),
        (
            Some("db.example.com"),
            "wamn-pg",
            "platform",
            "db.example.com",
        ),
    ] {
        let host = database_host(explicit, cluster, namespace);
        let url = workload_url(
            "postgresql://postgres:admin@127.0.0.1:15432/postgres?sslmode=disable",
            &host,
            5432,
            "wamn_role",
            "p@ss",
            "wamn-db-dkk--receiving--dev--4pqjfmli",
        )
        .expect("the URL builds");
        assert_eq!(
            url,
            format!(
                "postgresql://wamn_role:p%40ss@{expected}:5432/wamn-db-dkk--receiving--dev--4pqjfmli"
            )
        );
        let cdc_url = wamn_control_provision::compose_url(
            "replication",
            "private",
            &host,
            5432,
            "project_database",
        );
        assert_eq!(Url::parse(&cdc_url).unwrap().host_str(), Some(expected));
    }
}

/// `env apply` passes no marker, so a row `wamn dev up` marked disposable
/// stays disposable in the registry and in its projection (wamn-p9xix).
#[tokio::test]
async fn a_record_without_a_marker_keeps_the_disposable_row() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    client.batch_execute("BEGIN; SET LOCAL app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs (id,placement_type) VALUES ('demo','dedicated'); INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('demo','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18'); COMMIT;").await.unwrap();
    let triple = Triple::new("demo", "inventory", "dev");
    for disposable in [Some(true), None] {
        do_record_project_env(
            &mut client,
            &triple,
            Some("org-a"),
            None,
            "cloud",
            "db-first",
            Some("system"),
            "abcd1234",
            disposable,
            &wamn_broker::Publisher::default(),
        )
        .await
        .unwrap();
    }
    let registry: bool = client
        .query_one(
            "SELECT disposable FROM registry.project_envs WHERE org='demo' AND project='inventory' AND env='dev'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(registry, "a record without a marker keeps the registry row");
    client
        .batch_execute("SET app.project_env_key='org-a'")
        .await
        .unwrap();
    let projected: bool = client
        .query_one(
            "SELECT disposable FROM catalog.project_environments WHERE project_env_key='org-a'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(projected, "the projection follows the stored row");
}

/// A run with only PAT outputs mints for a recorded environment and writes
/// nothing to the registry, so it refuses the two registry values (wamn-rjtf).
#[tokio::test(flavor = "current_thread")]
async fn a_pat_only_run_refuses_the_registry_values_of_a_provisioning_run() {
    for (secret_namespace, disposable) in [(Some("hosts".to_owned()), None), (None, Some(true))] {
        let error = provision_project_env(&ProvisionProjectEnvRequest {
            changes: wamn_broker::Publisher::default(),
            org: "acme".into(),
            project: "billing".into(),
            env: "dev".into(),
            policy: None,
            target: "cloud".to_owned(),
            disposable,
            system_database_url: Some("postgres://unused.invalid/wamn_system".into()),
            cluster: None,
            connection_limit: None,
            cluster_namespace: "platform".into(),
            namespace: "hosts".into(),
            secret_namespace,
            emit_database: None,
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
            pat_issuer: PatIssuerConfig::default(),
            emit_management_author_pat_secret: None,
            emit_operator_pat_secret: Some("/tmp/wamn-rjtf-unused.json".into()),
        })
        .await
        .expect_err("a PAT-only run must refuse a registry value");
        assert_eq!(
            error.to_string(),
            "--secret-namespace and --disposable belong to a provisioning run, which passes an --emit-* output"
        );
    }
}
