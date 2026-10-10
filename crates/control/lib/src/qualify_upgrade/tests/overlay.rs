//! Atomic base successors retain overlay data and consumed contracts.

use super::*;

const OVERLAY: &str = "platform_fixture_overlay";
const ORIGINAL_PIN: &str =
    "sha256:7ed4955d0d23eb8ebd9752347755c77fdd02a48a682fd36f55df9385487fd6cc";

struct OverlayFixture {
    base: Fixture,
    predecessor: PathBuf,
    candidate: PathBuf,
    component: PathBuf,
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn compile(root: &Path) {
    fs::write(
        wamn_schema_generator::package_manifest_path(root),
        wamn_schema_generator::compile_manifest(root).unwrap(),
    )
    .unwrap();
}

fn frozen_predecessor(source: &Path, target: &Path) {
    for directory in [
        "migrations",
        "generated/contracts",
        "generated/platform-policy",
    ] {
        copy_tree(&source.join(directory), &target.join(directory));
    }
    for relative in [
        "wamn.k",
        "generated/wamn.json",
        "generated/package-identity.json",
    ] {
        fs::copy(source.join(relative), target.join(relative)).unwrap();
    }
    let manifest = PackageManifest::from_slice(
        &fs::read(wamn_schema_generator::package_manifest_path(source)).unwrap(),
    )
    .unwrap();
    for statements in crate::push_component::load_package_statement_facts(source, &manifest)
        .unwrap()
        .values()
    {
        for statement in statements.values() {
            let path = target.join(&statement.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::copy(source.join(&statement.path), path).unwrap();
        }
    }
    assert!(!target.join("generated/fixture-tui").exists());
    assert!(!target.join("generated/fixture_overlay-tui").exists());
}

async fn overlay_fixture(name: &str) -> OverlayFixture {
    let mut base = fixture(name, false).await;
    let predecessor = base.root.join("overlay-predecessor");
    copy_tree(
        &wamn_fixture_package::package_root()
            .parent()
            .unwrap()
            .join("platform_fixture_overlay"),
        &predecessor,
    );
    let workspace_manifest = predecessor.join("generated/fixture_overlay-tui/Cargo.toml");
    let declaration = fs::read_to_string(&workspace_manifest).unwrap();
    let workspace_entry = "workspace = \"../../../..\"";
    assert_eq!(declaration.matches(workspace_entry).count(), 1);
    let workspace = serde_json::to_string(&wamn_fixture_package::repository_root()).unwrap();
    fs::write(
        workspace_manifest,
        declaration.replacen(workspace_entry, &format!("workspace = {workspace}"), 1),
    )
    .unwrap();
    crate::apply_package::apply_package(apply_request(base.source_database.url(), &predecessor))
        .await
        .unwrap();
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: vec![wamn_fixture_package::package_root(), predecessor.clone()],
        database_url: base.source_database.url().to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .unwrap();
    base.source.batch_execute(
        "BEGIN; SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
         set_config('app.operation', 'admin:seed-overlay-proof', true); \
         UPDATE inventory.widget SET overlay_note = 'retained overlay value' WHERE code = 'priority'; COMMIT;",
    ).await.unwrap();

    let mut components = base.manifest.components.clone();
    let mut component = components.pop_first().unwrap();
    component.digest = ArtifactHash::parse(ORIGINAL_PIN).unwrap();
    components.insert(component);
    let overlay_manifest = PackageManifest::from_slice(
        &fs::read(wamn_schema_generator::package_manifest_path(&predecessor)).unwrap(),
    )
    .unwrap();
    let base_manifest =
        PackageManifest::from_slice(&wamn_fixture_package::manifest_bytes()).unwrap();
    let catalog =
        wamn_schema_generator::introspect_package(base.source_database.url(), &predecessor)
            .await
            .unwrap();
    let projected = wamn_schema_generator::project_package_catalog(
        &catalog,
        &overlay_manifest,
        &[base_manifest, overlay_manifest.clone()],
    )
    .unwrap();
    wamn_schema_generator::materialize_package_verified_with_existing_grants(
        MaterializeMode::Write,
        &projected,
        base.source_database.url(),
        &predecessor,
        "inventory",
    )
    .await
    .unwrap();
    let operations =
        crate::push_component::load_package_statement_facts(&predecessor, &overlay_manifest)
            .unwrap()
            .into_iter()
            .map(|(name, statements)| {
                (
                    name.clone(),
                    ServingComponentOperation {
                        pre_commit: None,
                        registered_operation: Some(name.clone()),
                        permissions: BTreeSet::from([name]),
                        fresh_only: false,
                        committed_result_schema: None,
                        participant: None,
                        statements,
                    },
                )
            })
            .collect();
    components.insert(ServingComponent {
        package_id: OVERLAY.to_owned(),
        component: "fixture_overlay".to_owned(),
        interface_version: "0.1.0".to_owned(),
        digest: ArtifactHash::parse(format!("sha256:{}", "c".repeat(64))).unwrap(),
        operations,
    });
    let mut release = base.manifest.release.clone();
    release
        .packages
        .insert(PackageCoordinate::new(OVERLAY, PREDECESSOR_VERSION).unwrap());
    base.manifest = ServingManifest::new(
        release,
        components,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let bytes = base.manifest.canonical_bytes();
    let digest = base.manifest.digest().as_str().to_owned();
    base.source.execute(
        "INSERT INTO catalog.releases (tenant_id,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)",
        &[&TENANT, &digest, &bytes],
    ).await.unwrap();
    base.serving.manifest_digest = digest;
    base.serving.packages.insert(
        OVERLAY.to_owned(),
        base.serving.packages["platform_fixture"].clone(),
    );

    let component = base.root.join("candidate-base.wasm");
    fs::write(&component, b"qualified fixture successor component").unwrap();
    let pin = wamn_engine::component_admission::component_digest(&fs::read(&component).unwrap());
    let candidate = base.root.join("overlay-candidate");
    copy_tree(&predecessor, &candidate);
    let declaration_path = candidate.join("wamn.k");
    let declaration = fs::read_to_string(&declaration_path)
        .unwrap()
        .replace(
            "version = \"2.1.0\", predecessor_version = \"2.0.0\"",
            "version = \"2.2.0\", predecessor_version = \"2.1.0\"",
        )
        .replace(
            "            version = \"2.1.0\"",
            "            version = \"2.2.0\"",
        )
        .replace(ORIGINAL_PIN, &pin);
    fs::write(declaration_path, declaration).unwrap();
    compile(&candidate);
    let generation = base.server.create_database("overlay_generation").unwrap();
    install(&connect(generation.url()).await).await;
    apply_qualification_package(apply_request(
        generation.url(),
        &base.root.join("candidate"),
    ))
    .await
    .unwrap();
    apply_qualification_package(apply_request(generation.url(), &candidate))
        .await
        .unwrap();
    wamn_schema_generator::materialize_package_verified(
        MaterializeMode::Write,
        generation.url(),
        &candidate,
    )
    .await
    .unwrap();
    OverlayFixture {
        base,
        predecessor,
        candidate,
        component,
    }
}

fn overlay_request(fixture: &OverlayFixture, result: &str) -> QualifyUpgradeRequest {
    let mut request = request(&fixture.base, result);
    request.predecessor_packages = vec![
        wamn_fixture_package::package_root(),
        fixture.predecessor.clone(),
    ];
    request.presented_packages.push(fixture.candidate.clone());
    request.base_component = Some(fixture.component.clone());
    request
        .workload
        .package_workloads
        .insert(OVERLAY.to_owned(), "http".to_owned());
    request
}

async fn apply_set(
    fixture: &OverlayFixture,
    evidence: &Path,
) -> anyhow::Result<Vec<crate::apply_package::ApplyOutcome>> {
    crate::apply_package::apply_qualified_package_set(
        apply_request(
            fixture.base.source_database.url(),
            &fixture.base.root.join("candidate"),
        ),
        &[
            fixture.base.root.join("candidate"),
            fixture.candidate.clone(),
        ],
        evidence,
    )
    .await
}

#[tokio::test]
async fn nontransactional_candidate_statements_are_named_before_mutation() {
    let mut fixture = overlay_fixture("overlay-nontransactional").await;
    let original = state(&mut fixture.base.source).await;
    for (name, statement) in [
        (
            "CREATE INDEX CONCURRENTLY",
            "CREATE INDEX CONCURRENTLY upgrade_widget_code ON inventory.widget(code);",
        ),
        (
            "DROP INDEX CONCURRENTLY",
            "DROP INDEX CONCURRENTLY inventory.upgrade_widget_code;",
        ),
        (
            "REINDEX CONCURRENTLY",
            "REINDEX INDEX CONCURRENTLY inventory.upgrade_widget_code;",
        ),
        ("VACUUM", "VACUUM inventory.widget;"),
        ("CREATE DATABASE", "CREATE DATABASE upgrade_forbidden;"),
        ("ALTER SYSTEM", "ALTER SYSTEM SET work_mem = '4MB';"),
    ] {
        fs::write(
            &fixture.base.suffix,
            format!("ALTER TABLE inventory.widget_maker ADD COLUMN note text;\n{statement}\n"),
        )
        .unwrap();
        let qualification = overlay_request(&fixture, "nontransactional.json");
        let result = qualification.result.clone();
        let refused = qualify_upgrade_with_observer(
            qualification,
            WorkloadObserver::Captured(fixture.base.serving.clone()),
        )
        .await
        .unwrap_err();
        let unapplied = crate::apply_package::apply_package(apply_request(
            fixture.base.source_database.url(),
            &fixture.base.root.join("candidate"),
        ))
        .await
        .unwrap_err();
        for error in [refused, unapplied] {
            let policy = error.chain().find_map(|source| {
                source.downcast_ref::<wamn_schema_introspection::migration_policy::MigrationPolicyError>()
            }).unwrap_or_else(|| panic!("{name}: {error:#}"));
            assert_eq!(policy.error_type(), wamn_schema_introspection::migration_policy::MigrationPolicyErrorType::NontransactionalOperation, "{name}: {error:#}");
            assert_eq!(policy.statement_index(), Some(2), "{name}: {error:#}");
            assert_eq!(policy.path(), fixture.base.suffix);
            assert!(format!("{error:#}").contains(name), "{name}: {error:#}");
        }
        assert!(!result.exists(), "{name} emitted qualification evidence");
        assert_eq!(state(&mut fixture.base.source).await, original, "{name}");
        let added: bool = fixture.base.source.query_one(
            "SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema = 'inventory' AND table_name = 'widget_maker' AND column_name = 'note')", &[],
        ).await.unwrap().get(0);
        assert!(!added, "{name} applied the preceding additive statement");
    }
}

#[tokio::test]
async fn unqualified_overlay_repin_preserves_installed_state() {
    let mut fixture = overlay_fixture("overlay-unqualified-repin").await;
    let original = state(&mut fixture.base.source).await;
    let predecessor = read_package_directory(&fixture.predecessor).unwrap();
    let candidate = read_package_directory(&fixture.candidate).unwrap();
    assert_eq!(candidate.migrations, predecessor.migrations);
    let error = crate::apply_package::apply_package(apply_request(
        fixture.base.source_database.url(),
        &fixture.candidate,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains(
            "installed overlay successors require coordinated base upgrade qualification"
        ),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.base.source).await, original);
}

#[tokio::test]
async fn qualification_refuses_input_changes_before_publication() {
    let mut fixture = overlay_fixture("overlay-input-changes").await;
    let predecessor = fixture.base.root.join("frozen-base-predecessor");
    frozen_predecessor(&wamn_fixture_package::package_root(), &predecessor);
    let original = state(&mut fixture.base.source).await;
    let mut accepted = overlay_request(&fixture, "unchanged.json");
    accepted.predecessor_packages[0] = predecessor.clone();
    qualify_upgrade_with_observer(
        accepted,
        WorkloadObserver::CapturedWithChange {
            serving: fixture.base.serving.clone(),
            before_publication: Box::new(|| {}),
        },
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.base.source).await, original);
    let append = |path: PathBuf| {
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(b'\n');
        (path, bytes)
    };
    let contract = |path: PathBuf| {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["properties"]["id"]["type"] = serde_json::json!("integer");
        (path, serde_json::to_vec(&value).unwrap())
    };
    let original_contract = predecessor.join("generated/contracts/widget/archive.input.json");
    let candidate_contract = fixture
        .base
        .root
        .join("candidate/generated/contracts/widget/archive.input.json");
    let identity = fixture.predecessor.join("generated/package-identity.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity).unwrap()).unwrap();
    metadata["verified_schema_state_id"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    let changed_identity = wamn_execution_contract::canonical_json_bytes(&metadata);
    let migration = fs::read_dir(fixture.predecessor.join("migrations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    for (name, changes, expected) in [
        (
            "manifest",
            vec![append(fixture.predecessor.join("generated/wamn.json"))],
            "presented package changed during qualification",
        ),
        (
            "migration",
            vec![append(migration)],
            "presented package changed during qualification",
        ),
        (
            "identity",
            vec![(identity, changed_identity)],
            "presented generated or SQL bytes changed during qualification",
        ),
        (
            "sql",
            vec![append(
                fixture.predecessor.join("generated/sql/widget/get.sql"),
            )],
            "statement-digest-mismatch",
        ),
        (
            "contract",
            vec![contract(original_contract.clone())],
            "changes its contract",
        ),
        (
            "both-contracts",
            vec![contract(original_contract), contract(candidate_contract)],
            "consumed contracts or base component bytes changed during qualification",
        ),
        (
            "component",
            vec![(fixture.component.clone(), b"changed component".to_vec())],
            "overlay successor must change only its package coordinate and affected base version/digest",
        ),
    ] {
        let restore = changes
            .iter()
            .map(|(path, _)| (path.clone(), fs::read(path).unwrap()))
            .collect::<Vec<_>>();
        let mut qualification = overlay_request(&fixture, &format!("changed-{name}.json"));
        qualification.predecessor_packages[0] = predecessor.clone();
        let result = qualification.result.clone();
        let error = qualify_upgrade_with_observer(
            qualification,
            WorkloadObserver::CapturedWithChange {
                serving: fixture.base.serving.clone(),
                before_publication: Box::new(move || {
                    for (path, bytes) in changes {
                        fs::write(path, bytes).unwrap();
                    }
                }),
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{name}: {error:#}");
        assert!(!result.exists(), "{name} emitted qualification evidence");
        assert_eq!(state(&mut fixture.base.source).await, original, "{name}");
        for (path, bytes) in restore {
            fs::write(path, bytes).unwrap();
        }
    }
}

#[tokio::test]
async fn atomic_overlay_successor_retains_rows_constraints_and_rollback() {
    let mut fixture = overlay_fixture("overlay-retained").await;
    let original = state(&mut fixture.base.source).await;
    let rows = retained_rows(&fixture.base.source).await;
    let base_predecessor = fixture.base.root.join("frozen-base-predecessor");
    let overlay_predecessor = fixture.base.root.join("frozen-overlay-predecessor");
    frozen_predecessor(&wamn_fixture_package::package_root(), &base_predecessor);
    frozen_predecessor(&fixture.predecessor, &overlay_predecessor);
    let mut qualification = overlay_request(&fixture, "accepted.json");
    qualification.predecessor_packages = vec![base_predecessor, overlay_predecessor];
    let result = qualify_upgrade_with_observer(
        qualification,
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.base.source).await, original);
    let applied = apply_set(&fixture, &result.result).await.unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].package_id, "platform_fixture");
    assert_eq!(applied[0].migrations_applied, 1);
    assert_eq!(applied[1].package_id, OVERLAY);
    assert_eq!(applied[1].migrations_applied, 0);
    assert_eq!(retained_rows(&fixture.base.source).await, rows);
    let overlay_note: String = fixture
        .base
        .source
        .query_one(
            "SELECT overlay_note FROM inventory.widget WHERE code = 'priority'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(overlay_note, "retained overlay value");
    crate::reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessRequest {
            packages: vec![
                fixture.base.root.join("candidate"),
                fixture.candidate.clone(),
            ],
            database_url: fixture.base.source_database.url().to_owned(),
            tenant: TENANT.to_owned(),
        },
    )
    .await
    .unwrap();
    let reconciled = state(&mut fixture.base.source).await;
    let replay = apply_set(&fixture, &result.result).await.unwrap();
    assert!(
        replay
            .iter()
            .all(|outcome| !outcome.changed && outcome.migrations_applied == 0)
    );
    assert_eq!(state(&mut fixture.base.source).await, reconciled);
    // Exercise the environment stage's production retry against the retained database.
    let packages = [
        fixture.base.root.join("candidate"),
        fixture.candidate.clone(),
    ]
    .into_iter()
    .map(|root| crate::release_composition::PackageInput {
        manifest: PackageManifest::from_slice(
            &fs::read(wamn_schema_generator::package_manifest_path(&root)).unwrap(),
        )
        .unwrap(),
        root,
    })
    .collect::<Vec<_>>();
    let artifacts = vec![crate::release_composition::SelectedComponentArtifact {
        package_id: "platform_fixture".into(),
        package_version: packages[0].manifest.package.version.clone().into(),
        component: "fixture".into(),
        path: fixture.component.clone(),
        digest: wamn_engine::component_admission::component_digest(
            &fs::read(&fixture.component).unwrap(),
        )
        .into(),
    }];
    let environment_retry = |packages, artifacts| {
        crate::upgrade_environment::apply_environment_package_qualification(
            apply_request(
                fixture.base.source_database.url(),
                &fixture.base.root.join("candidate"),
            ),
            ENVIRONMENT,
            packages,
            artifacts,
            &result.result,
        )
    };
    let replay = environment_retry(&packages, &artifacts).await.unwrap();
    assert_eq!(replay.len(), 2);
    assert!(
        replay
            .iter()
            .all(|outcome| !outcome.changed && outcome.migrations_applied == 0)
    );
    let error = environment_retry(&packages[..1], &artifacts)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("complete successor root set"),
        "{error:#}"
    );
    let mut changed_artifacts = artifacts.clone();
    changed_artifacts[0].digest = format!("sha256:{}", "f".repeat(64)).into();
    let error = environment_retry(&packages, &changed_artifacts)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("qualified overlay pin"),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.base.source).await, reconciled);
    let tx = fixture.base.source.transaction().await.unwrap();
    tx.batch_execute("SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), set_config('app.operation', 'admin:constraint-proof', true)").await.unwrap();
    let duplicate = tx
        .execute(
            "INSERT INTO inventory.widget (code) VALUES ('priority')",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    tx.rollback().await.unwrap();
    for (statement, expected) in [
        (
            "UPDATE inventory.widget SET code = 'invalid' WHERE code = 'priority'",
            tokio_postgres::error::SqlState::CHECK_VIOLATION,
        ),
        (
            "UPDATE inventory.widget SET maker_id = '00000000-0000-4000-8000-000000000099' WHERE code = 'priority'",
            tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION,
        ),
    ] {
        let tx = fixture.base.source.transaction().await.unwrap();
        tx.batch_execute("SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), set_config('app.operation', 'admin:constraint-proof', true)").await.unwrap();
        let error = tx.execute(statement, &[]).await.unwrap_err();
        assert_eq!(error.code(), Some(&expected));
        tx.rollback().await.unwrap();
    }
    assert_eq!(retained_rows(&fixture.base.source).await, rows);
}

#[tokio::test]
async fn single_package_accepts_frozen_predecessor_artifacts() {
    let mut fixture = fixture("single-frozen-predecessor", false).await;
    let predecessor = fixture.root.join("frozen-predecessor");
    frozen_predecessor(&wamn_fixture_package::package_root(), &predecessor);
    let original = state(&mut fixture.source).await;
    let mut qualification = request(&fixture, "accepted.json");
    qualification.predecessor_packages = vec![predecessor];
    qualify_upgrade_with_observer(
        qualification,
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
}

#[tokio::test]
async fn incomplete_or_wrong_overlay_pin_preserves_source() {
    let mut fixture = overlay_fixture("overlay-refusals").await;
    let original = state(&mut fixture.base.source).await;
    let manifest_path = wamn_schema_generator::package_manifest_path(&fixture.predecessor);
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    changed["package"]["predecessor_version"] = serde_json::json!("0.9.0");
    fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "original-manifest-mismatch.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("manifest identity mismatch")
            && format!("{error:#}").contains(OVERLAY),
        "{error:#}"
    );
    fs::write(&manifest_path, manifest_bytes).unwrap();
    let migration = fs::read_dir(fixture.predecessor.join("migrations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let migration_bytes = fs::read(&migration).unwrap();
    let mut changed = migration_bytes.clone();
    changed.extend_from_slice(b"\n");
    fs::write(&migration, changed).unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "original-migration-mismatch.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("migration identity mismatch")
            && format!("{error:#}").contains(OVERLAY)
            && format!("{error:#}").contains(migration.file_name().unwrap().to_str().unwrap()),
        "{error:#}"
    );
    fs::write(&migration, migration_bytes).unwrap();
    assert!(
        !fixture
            .base
            .root
            .join("original-manifest-mismatch.json")
            .exists()
    );
    assert!(
        !fixture
            .base
            .root
            .join("original-migration-mismatch.json")
            .exists()
    );
    assert_eq!(state(&mut fixture.base.source).await, original);
    let sql_path = fixture.predecessor.join("generated/sql/widget/get.sql");
    let contract_path = fixture
        .predecessor
        .join("generated/contracts/widget/get.operation.json");
    let identity_path = fixture.predecessor.join("generated/package-identity.json");
    let sql = fs::read(&sql_path).unwrap();
    let contract = fs::read(&contract_path).unwrap();
    let identity_bytes = fs::read(&identity_path).unwrap();
    let mut changed_sql = sql.clone();
    changed_sql.extend_from_slice(b"\n-- different carried predecessor statement\n");
    let mut changed_contract: serde_json::Value = serde_json::from_slice(&contract).unwrap();
    changed_contract["statements"][0]["digest"] = serde_json::json!(
        wamn_engine::component_admission::component_digest(&changed_sql)
    );
    let mut changed_identity: serde_json::Value = serde_json::from_slice(&identity_bytes).unwrap();
    changed_identity["application_sql_corpus_identity"] =
        serde_json::json!(wamn_schema_generator::corpus_sha256([(
            "generated/sql/widget/get.sql",
            changed_sql.as_slice()
        )]));
    fs::write(&sql_path, changed_sql).unwrap();
    fs::write(
        &contract_path,
        serde_json::to_vec(&changed_contract).unwrap(),
    )
    .unwrap();
    fs::write(
        &identity_path,
        serde_json::to_vec(&changed_identity).unwrap(),
    )
    .unwrap();
    let manifest = PackageManifest::from_slice(
        &fs::read(wamn_schema_generator::package_manifest_path(
            &fixture.predecessor,
        ))
        .unwrap(),
    )
    .unwrap();
    crate::push_component::load_package_statement_facts(&fixture.predecessor, &manifest).unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "original-serving-sql-mismatch.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("frozen predecessor statement differs from serving manifest")
            && format!("{error:#}").contains("platform-fixture-overlay:widget/get@2.1.0"),
        "{error:#}"
    );
    fs::write(sql_path, sql).unwrap();
    fs::write(contract_path, contract).unwrap();
    fs::write(identity_path, identity_bytes).unwrap();
    assert!(
        !fixture
            .base
            .root
            .join("original-serving-sql-mismatch.json")
            .exists()
    );
    assert_eq!(state(&mut fixture.base.source).await, original);
    let mut base_only = request(&fixture.base, "base-only.json");
    base_only
        .presented_packages
        .push(fixture.predecessor.clone());
    base_only
        .workload
        .package_workloads
        .insert(OVERLAY.to_owned(), "http".to_owned());
    let error = qualify_upgrade_with_observer(
        base_only,
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("base-only") && format!("{error:#}").contains(OVERLAY),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.base.source).await, original);
    let mut omitted = overlay_request(&fixture, "omitted.json");
    omitted.presented_packages.pop();
    omitted.workload.package_workloads.remove(OVERLAY);
    let error = qualify_upgrade_with_observer(
        omitted,
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains(OVERLAY), "{error:#}");
    assert!(!fixture.base.root.join("omitted.json").exists());
    assert_eq!(state(&mut fixture.base.source).await, original);
    let component = fs::read(&fixture.component).unwrap();
    fs::write(&fixture.component, b"different base component").unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "wrong-pin.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("digest"), "{error:#}");
    fs::write(&fixture.component, component).unwrap();
    assert!(!fixture.base.root.join("wrong-pin.json").exists());
    assert_eq!(state(&mut fixture.base.source).await, original);

    let contract = fixture
        .base
        .root
        .join("candidate/generated/contracts/widget/archive.input.json");
    let bytes = fs::read(&contract).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    changed["properties"]["id"]["type"] = serde_json::json!("integer");
    fs::write(&contract, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "changed-contract.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("widget.archive")
            && format!("{error:#}").contains("contract"),
        "{error:#}"
    );
    fs::write(contract, bytes).unwrap();
    assert!(!fixture.base.root.join("changed-contract.json").exists());
    assert_eq!(state(&mut fixture.base.source).await, original);
    let suffix = fs::read(&fixture.base.suffix).unwrap();
    fs::write(
        &fixture.base.suffix,
        "CREATE INDEX CONCURRENTLY upgrade_widget_code ON inventory.widget(code);\n",
    )
    .unwrap();
    let error = qualify_upgrade_with_observer(
        overlay_request(&fixture, "nontransactional.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(error.chain().any(|source| source.downcast_ref::<wamn_schema_introspection::migration_policy::MigrationPolicyError>()
        .is_some_and(|error| error.error_type() == wamn_schema_introspection::migration_policy::MigrationPolicyErrorType::NontransactionalOperation)), "{error:#}");
    assert!(format!("{error:#}").contains("CONCURRENTLY"), "{error:#}");
    fs::write(&fixture.base.suffix, suffix).unwrap();
    assert!(!fixture.base.root.join("nontransactional.json").exists());
    assert_eq!(state(&mut fixture.base.source).await, original);
}

#[tokio::test]
async fn second_package_failure_rolls_back_base_and_evidence() {
    let mut fixture = overlay_fixture("overlay-atomic-failure").await;
    let result = qualify_upgrade_with_observer(
        overlay_request(&fixture, "accepted.json"),
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap();
    fixture.base.source.batch_execute(
        "CREATE FUNCTION catalog.refuse_overlay_successor() RETURNS trigger LANGUAGE plpgsql AS $body$ \
         BEGIN IF NEW.package_id = 'platform_fixture_overlay' AND NEW.package_version = '2.2.0' \
         THEN RAISE EXCEPTION 'test second package registration failure'; END IF; RETURN NEW; END $body$; \
         CREATE TRIGGER refuse_overlay_successor BEFORE INSERT ON catalog.packages \
         FOR EACH ROW EXECUTE FUNCTION catalog.refuse_overlay_successor();",
    ).await.unwrap();
    let original = state(&mut fixture.base.source).await;
    let error = apply_set(&fixture, &result.result).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("test second package registration failure"),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.base.source).await, original);
    let added: bool = fixture.base.source.query_one(
        "SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema = 'inventory' AND table_name = 'widget_maker' AND column_name = 'note')", &[],
    ).await.unwrap().get(0);
    assert!(!added);
    fixture.base.source.batch_execute("DROP TRIGGER refuse_overlay_successor ON catalog.packages; DROP FUNCTION catalog.refuse_overlay_successor()").await.unwrap();
    assert_eq!(apply_set(&fixture, &result.result).await.unwrap().len(), 2);
}

#[tokio::test]
async fn every_affected_overlay_is_required_and_applied_atomically() {
    const SECOND: &str = "platform_fixture_overlay_second";
    let mut fixture = overlay_fixture("two-overlays").await;
    let predecessor = fixture.base.root.join("second-predecessor");
    let candidate = fixture.base.root.join("second-candidate");
    for (source, target) in [
        (&fixture.predecessor, &predecessor),
        (&fixture.candidate, &candidate),
    ] {
        copy_tree(source, target);
        let declaration = target.join("wamn.k");
        fs::write(
            &declaration,
            fs::read_to_string(&declaration)
                .unwrap()
                .replace(OVERLAY, SECOND)
                .replace("overlay_note", "secondary_note"),
        )
        .unwrap();
        for migration in fs::read_dir(target.join("migrations")).unwrap() {
            let migration = migration.unwrap().path();
            fs::write(
                &migration,
                fs::read_to_string(&migration)
                    .unwrap()
                    .replace("overlay_note", "secondary_note"),
            )
            .unwrap();
        }
        let workspace_manifest = target.join("generated/fixture_overlay-tui/Cargo.toml");
        let workspace = fs::read(&workspace_manifest).unwrap();
        fs::remove_dir_all(target.join("generated")).unwrap();
        fs::create_dir_all(workspace_manifest.parent().unwrap()).unwrap();
        fs::write(workspace_manifest, workspace).unwrap();
        compile(target);
    }
    let original_roots = vec![
        wamn_fixture_package::package_root(),
        fixture.predecessor.clone(),
        predecessor.clone(),
    ];
    let successor_roots = vec![
        fixture.base.root.join("candidate"),
        fixture.candidate.clone(),
        candidate.clone(),
    ];
    crate::apply_package::apply_package(apply_request(
        fixture.base.source_database.url(),
        &predecessor,
    ))
    .await
    .unwrap();
    let installed = original_roots
        .iter()
        .map(|root| {
            PackageManifest::from_slice(
                &fs::read(wamn_schema_generator::package_manifest_path(root)).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let catalog =
        wamn_schema_generator::introspect_package(fixture.base.source_database.url(), &predecessor)
            .await
            .unwrap();
    let manifest = installed
        .iter()
        .find(|manifest| manifest.package.id == SECOND)
        .unwrap();
    let projected =
        wamn_schema_generator::project_package_catalog(&catalog, manifest, &installed).unwrap();
    wamn_schema_generator::materialize_package_from_catalog(
        MaterializeMode::Write,
        &projected,
        &predecessor,
    )
    .unwrap();
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: original_roots.clone(),
        database_url: fixture.base.source_database.url().to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .unwrap();
    wamn_schema_generator::materialize_package_verified_with_existing_grants(
        MaterializeMode::Write,
        &projected,
        fixture.base.source_database.url(),
        &predecessor,
        "inventory",
    )
    .await
    .unwrap();
    fixture.base.source.batch_execute(
        "BEGIN; SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
         set_config('app.operation', 'admin:seed-second-overlay', true); \
         UPDATE inventory.widget SET secondary_note = 'second retained value' WHERE code = 'priority'; COMMIT;",
    ).await.unwrap();
    let manifest = PackageManifest::from_slice(
        &fs::read(wamn_schema_generator::package_manifest_path(&predecessor)).unwrap(),
    )
    .unwrap();
    let operations = crate::push_component::load_package_statement_facts(&predecessor, &manifest)
        .unwrap()
        .into_iter()
        .map(|(name, statements)| {
            (
                name.clone(),
                ServingComponentOperation {
                    pre_commit: None,
                    registered_operation: Some(name.clone()),
                    permissions: BTreeSet::from([name]),
                    fresh_only: false,
                    committed_result_schema: None,
                    participant: None,
                    statements,
                },
            )
        })
        .collect();
    let mut components = fixture.base.manifest.components.clone();
    components.insert(ServingComponent {
        package_id: SECOND.to_owned(),
        component: "fixture_overlay".to_owned(),
        interface_version: "0.1.0".to_owned(),
        digest: ArtifactHash::parse(format!("sha256:{}", "d".repeat(64))).unwrap(),
        operations,
    });
    let mut release = fixture.base.manifest.release.clone();
    release
        .packages
        .insert(PackageCoordinate::new(SECOND, PREDECESSOR_VERSION).unwrap());
    fixture.base.manifest = ServingManifest::new(
        release,
        components,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let bytes = fixture.base.manifest.canonical_bytes();
    let digest = fixture.base.manifest.digest().as_str().to_owned();
    fixture.base.source.execute(
        "INSERT INTO catalog.releases (tenant_id,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)",
        &[&TENANT, &digest, &bytes],
    ).await.unwrap();
    fixture.base.serving.manifest_digest = digest;
    fixture.base.serving.packages.insert(
        SECOND.to_owned(),
        fixture.base.serving.packages[OVERLAY].clone(),
    );
    let generation = fixture
        .base
        .server
        .create_database("two_overlay_generation")
        .unwrap();
    install(&connect(generation.url()).await).await;
    for package in &successor_roots {
        apply_qualification_package(apply_request(generation.url(), package))
            .await
            .unwrap();
    }
    let installed = successor_roots
        .iter()
        .map(|root| {
            PackageManifest::from_slice(
                &fs::read(wamn_schema_generator::package_manifest_path(root)).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let catalog = wamn_schema_generator::introspect_package(generation.url(), &candidate)
        .await
        .unwrap();
    let manifest = installed
        .iter()
        .find(|manifest| manifest.package.id == SECOND)
        .unwrap();
    let projected =
        wamn_schema_generator::project_package_catalog(&catalog, manifest, &installed).unwrap();
    wamn_schema_generator::materialize_package_from_catalog(
        MaterializeMode::Write,
        &projected,
        &candidate,
    )
    .unwrap();
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: successor_roots.clone(),
        database_url: generation.url().to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .unwrap();
    wamn_schema_generator::materialize_package_verified_with_existing_grants(
        MaterializeMode::Write,
        &projected,
        generation.url(),
        &candidate,
        "inventory",
    )
    .await
    .unwrap();
    let mut request = overlay_request(&fixture, "two-accepted.json");
    request.predecessor_packages = original_roots;
    request.presented_packages = successor_roots.clone();
    request
        .workload
        .package_workloads
        .insert(SECOND.to_owned(), "http".to_owned());
    let original = state(&mut fixture.base.source).await;
    let rows = retained_rows(&fixture.base.source).await;
    let mut omitted = overlay_request(&fixture, "second-omitted.json");
    omitted.predecessor_packages = request.predecessor_packages.clone();
    let error = qualify_upgrade_with_observer(
        omitted,
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains(&format!(
            "successor roots omit affected installed overlay {SECOND}"
        )),
        "{error:#}"
    );
    assert!(!fixture.base.root.join("second-omitted.json").exists());
    assert_eq!(state(&mut fixture.base.source).await, original);
    let result = qualify_upgrade_with_observer(
        request,
        WorkloadObserver::Captured(fixture.base.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.base.source).await, original);
    let applied = crate::apply_package::apply_qualified_package_set(
        apply_request(
            fixture.base.source_database.url(),
            &fixture.base.root.join("candidate"),
        ),
        &successor_roots,
        &result.result,
    )
    .await
    .unwrap();
    assert_eq!(
        applied
            .iter()
            .map(|outcome| (outcome.package_id.as_str(), outcome.migrations_applied))
            .collect::<Vec<_>>(),
        vec![("platform_fixture", 1), (OVERLAY, 0), (SECOND, 0)]
    );
    assert_eq!(retained_rows(&fixture.base.source).await, rows);
    let retained = fixture
        .base
        .source
        .query_one(
            "SELECT overlay_note, secondary_note FROM inventory.widget WHERE code = 'priority'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(retained.get::<_, String>(0), "retained overlay value");
    assert_eq!(retained.get::<_, String>(1), "second retained value");
    let constraints: i64 = fixture.base.source.query_one(
        "SELECT count(*) FROM pg_constraint WHERE conrelid = 'inventory.widget'::regclass AND contype IN ('c','f','p','u')", &[],
    ).await.unwrap().get(0);
    assert_eq!(constraints, 4);
}
