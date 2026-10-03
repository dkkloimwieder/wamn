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
    release.effective_release_id = EffectiveReleaseId::new(2).unwrap();
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
        "INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment) VALUES ($1,2,$2)",
        &[&TENANT, &ENVIRONMENT],
    ).await.unwrap();
    base.source.execute(
        "INSERT INTO catalog.release_manifest_snapshots (tenant_id,effective_release_id,manifest_digest,canonical_bytes) VALUES ($1,2,$2,$3)",
        &[&TENANT, &digest, &bytes],
    ).await.unwrap();
    base.source.execute(
        "INSERT INTO catalog.effective_release_packages (tenant_id,effective_release_id,package_id,package_version) VALUES ($1,2,$2,$3)",
        &[&TENANT, &OVERLAY, &PREDECESSOR_VERSION],
    ).await.unwrap();
    base.source.execute(
        "INSERT INTO catalog.effective_release_packages (tenant_id,effective_release_id,package_id,package_version) VALUES ($1,2,'platform_fixture',$2)",
        &[&TENANT, &PREDECESSOR_VERSION],
    ).await.unwrap();
    base.source.execute(
        "UPDATE catalog.effective_release_heads SET effective_release_id = 2 WHERE tenant_id = $1 AND environment = $2",
        &[&TENANT, &ENVIRONMENT],
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
    crate::apply_package::apply_qualified_package_set_observed(
        apply_request(
            fixture.base.source_database.url(),
            &fixture.base.root.join("candidate"),
        ),
        &[
            fixture.base.root.join("candidate"),
            fixture.candidate.clone(),
        ],
        evidence,
        fixture.base.serving.clone(),
    )
    .await
}

#[tokio::test]
async fn atomic_overlay_successor_retains_rows_constraints_and_rollback() {
    let mut fixture = overlay_fixture("overlay-retained").await;
    let original = state(&mut fixture.base.source).await;
    let rows = retained_rows(&fixture.base.source).await;
    let result = qualify_upgrade_with_observer(
        overlay_request(&fixture, "accepted.json"),
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
    let mut tx = fixture.base.source.transaction().await.unwrap();
    crate::package_upgrade::require_compatible_schema(&mut tx, &fixture.base.manifest)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
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
    release.effective_release_id = EffectiveReleaseId::new(3).unwrap();
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
        "INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment) VALUES ($1,3,$2)",
        &[&TENANT, &ENVIRONMENT],
    ).await.unwrap();
    fixture.base.source.execute(
        "INSERT INTO catalog.release_manifest_snapshots (tenant_id,effective_release_id,manifest_digest,canonical_bytes) VALUES ($1,3,$2,$3)",
        &[&TENANT, &digest, &bytes],
    ).await.unwrap();
    for package in ["platform_fixture", OVERLAY, SECOND] {
        fixture.base.source.execute(
            "INSERT INTO catalog.effective_release_packages (tenant_id,effective_release_id,package_id,package_version) VALUES ($1,3,$2,$3)",
            &[&TENANT, &package, &PREDECESSOR_VERSION],
        ).await.unwrap();
    }
    fixture.base.source.execute(
        "UPDATE catalog.effective_release_heads SET effective_release_id = 3 WHERE tenant_id = $1 AND environment = $2",
        &[&TENANT, &ENVIRONMENT],
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
    let applied = crate::apply_package::apply_qualified_package_set_observed(
        apply_request(
            fixture.base.source_database.url(),
            &fixture.base.root.join("candidate"),
        ),
        &successor_roots,
        &result.result,
        fixture.base.serving.clone(),
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
