//! Receiving environment tests and helpers.

use super::*;

pub(super) fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving")
}

pub(super) fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
}

pub(super) fn journey_package_root(package: JourneyPackage) -> PathBuf {
    if package.id == BASE_PACKAGE_ID {
        package_root()
    } else {
        overlay_package_root()
    }
}

pub(super) fn required_journey(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("set {key} for the disposable Receiving route journey"))
}

pub(super) fn required_journey_path(key: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(required_journey(key)?))
}

pub(super) fn journey_publication_root(package: JourneyPackage) -> PathBuf {
    journey_package_root(package).join("publication")
}

pub(super) fn overlay_route_path(wiring_id: &str) -> &'static str {
    JOURNEY_ATTACHMENTS
        .iter()
        .find_map(|attachment| {
            (attachment.package_id == OVERLAY_PACKAGE_ID && attachment.wiring_id == wiring_id)
                .then_some(attachment.path)
        })
        .expect("every public overlay wiring has one deployment route")
}

/// Apply each released journey package as the artifact
/// [`push_journey_packages`] pushed, in its order.
pub(super) async fn install_journey_project(
    packages: &[wamn_control::apply_package::PackageRoot],
    project: &Client,
    project_url: &str,
) -> anyhow::Result<()> {
    install_journey_platform_floor(project, identity().tenant.as_str(), PLATFORM_DOMAIN).await?;
    for (package, root) in released_journey_packages().zip(packages) {
        apply_package::apply_package(ApplyPackageRequest {
            package: root.clone(),
            database_url: project_url.to_owned(),
            tenant: identity().tenant.clone(),
        })
        .await
        .with_context(|| {
            format!(
                "apply {}@{} through the exact-byte runner",
                package.id,
                package.version()
            )
        })?;
    }
    Ok(())
}

pub(super) async fn reconcile_journey_data_access(
    packages: &[wamn_control::apply_package::PackageRoot],
    project_url: &str,
) -> anyhow::Result<()> {
    wamn_gate_harness::environment::reconcile_package_data_access(
        ReconcilePackageDataAccessRequest {
            packages: packages.to_vec(),
            database_url: project_url.to_owned(),
            tenant: identity().tenant.clone(),
        },
    )
    .await
}

/// Applying the packages leaves the built-in `admin` role and no permission
/// row: `admin` holds every served operation without rows, and no other role
/// is authored yet (docs/plan/platform-ui.md §2.2).
pub(super) async fn verify_journey_operation_grants(project: &Client) -> anyhow::Result<()> {
    let roles = project
        .query(
            "SELECT name FROM app_system.roles WHERE tenant_id = $1 ORDER BY name COLLATE \"C\"",
            &[&identity().tenant.as_str()],
        )
        .await
        .context("read the installed roles")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let permissions: i64 = project
        .query_one(
            "SELECT count(*) FROM app_system.permissions WHERE tenant_id = $1",
            &[&identity().tenant.as_str()],
        )
        .await
        .context("count the installed permission rows")?
        .get(0);
    anyhow::ensure!(
        roles == [ADMIN_ROLE] && permissions == 0,
        "installed packages left roles {roles:?} and {permissions} permission rows, \
         not admin alone without rows"
    );
    Ok(())
}

pub(super) fn repository_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .context("resolve the repository root for the product command")
}

pub(super) struct JourneyComponentDeclaration {
    pub(super) package: JourneyPackage,
    pub(super) path: PathBuf,
}

pub(super) fn render_component_declarations(
    root: &Path,
    component_directory: &Path,
) -> anyhow::Result<Vec<JourneyComponentDeclaration>> {
    let output = root.join("component-declarations");
    std::fs::create_dir_all(&output).context("create rendered declaration directory")?;
    released_journey_packages()
        .map(|package| {
            let source = journey_publication_root(package)
                .join("components")
                .join(format!("{}.json.in", package.component));
            // Like the disposable dev coordinator, layer this run's exact
            // virtualized bytes over authored pins without editing the package.
            let package_root = journey_package_root(package);
            let mut base_digests =
                wamn_control::component_declaration::authored_base_digests(&package_root)
                    .with_context(|| format!("read {} base pins", package_root.display()))?;
            for base in released_journey_packages() {
                let coordinate = format!("{}@{}", base.id, base.version());
                if let Some(digest) = base_digests.get_mut(coordinate.as_str()) {
                    let artifact = component_directory.join(format!("{}.wasm", base.component));
                    let bytes = std::fs::read(&artifact)
                        .with_context(|| format!("read built base {}", artifact.display()))?;
                    *digest =
                        wamn_engine::component_admission::component_digest(&bytes).into_boxed_str();
                }
            }
            let declaration = wamn_control::component_declaration::render_declaration_document(
                &source,
                identity().tenant.as_str(),
                &base_digests,
            )
            .with_context(|| format!("render {}", source.display()))?;
            let destination = output.join(format!("{}.json", package.component));
            std::fs::write(&destination, serde_json::to_vec(&declaration)?)
                .with_context(|| format!("write {}", destination.display()))?;
            Ok(JourneyComponentDeclaration {
                package,
                path: destination,
            })
        })
        .collect()
}

#[test]
fn disposable_component_declarations_follow_built_base_bytes() -> anyhow::Result<()> {
    let root = ScratchRoot(
        std::env::temp_dir().join(format!("journey-built-base-digests-{}", std::process::id())),
    );
    std::fs::create_dir(root.path())?;
    let artifacts = root.path().join("components");
    std::fs::create_dir(&artifacts)?;
    let manifest = wamn_schema_generator::package_manifest_path(&overlay_package_root());
    let template = overlay_package_root()
        .join("publication/components")
        .join(format!("{OVERLAY_COMPONENT}.json.in"));
    let authored_manifest = std::fs::read(&manifest)?;
    let authored_template = std::fs::read(&template)?;
    assert!(render_component_declarations(root.path(), &artifacts).is_err());

    // Two distinct component binaries; the second adds an empty custom section.
    for bytes in [
        b"\0asm\x0d\0\x01\0".as_slice(),
        b"\0asm\x0d\0\x01\0\0\x02\x01x".as_slice(),
    ] {
        std::fs::write(artifacts.join(format!("{BASE_COMPONENT}.wasm")), bytes)?;
        let declarations = render_component_declarations(root.path(), &artifacts)?;
        let overlay = declarations
            .iter()
            .find(|declaration| declaration.package.id == OVERLAY_PACKAGE_ID)
            .expect("the journey renders its overlay");
        let declaration: Value = serde_json::from_slice(&std::fs::read(&overlay.path)?)?;
        assert_eq!(
            declaration["operations"][*OVERLAY_RECORD_RECEIPT]["dependencies"],
            serde_json::json!([{
                "package": BASE_PACKAGE_ID,
                "version": *BASE_PACKAGE_VERSION,
                "digest": wamn_engine::component_admission::component_digest(bytes),
                "operation": *BASE_RECORD_RECEIPT,
                "participant": *OVERLAY_RECEIPT_PARTICIPANT,
            }]),
        );
    }
    assert_eq!(std::fs::read(manifest)?, authored_manifest);
    assert_eq!(std::fs::read(template)?, authored_template);
    Ok(())
}

/// Admit each journey component from its exact bytes and declaration.
/// push-package pushes them with the package artifacts.
pub(super) fn admit_journey_components(
    inputs: &JourneyDocument,
    declarations: &[JourneyComponentDeclaration],
) -> anyhow::Result<Vec<AdmittedComponent>> {
    declarations
        .iter()
        .map(|declaration| {
            let package = declaration.package;
            let admission = admit_component(AdmitComponentRequest {
                package: journey_package_root(package),
                component_bytes: inputs
                    .component_directory
                    .join(format!("{}.wasm", package.component)),
                declaration: declaration.path.clone(),
                admitted_platform_packages: vec![
                    "wamn:node".to_owned(),
                    "wamn:postgres".to_owned(),
                ],
            })
            .with_context(|| {
                format!(
                    "admit production component {}@{}::{}",
                    package.id,
                    package.version(),
                    package.component
                )
            })?;
            Ok(admission.facts().clone())
        })
        .collect()
}

pub(super) fn verify_journey_components_are_effectful(
    components: &[AdmittedComponent],
) -> anyhow::Result<HashMap<String, String>> {
    let mut digests = HashMap::new();
    for package in released_journey_packages() {
        let admitted = components
            .iter()
            .filter(|component| {
                component.scope.package_id == package.id
                    && component.scope.package_version == package.version()
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            admitted.len() == 1,
            "component admission produced {} {} components instead of one",
            admitted.len(),
            package.id
        );
        let admitted = admitted[0];
        anyhow::ensure!(
            admitted.component == package.component,
            "component admission produced {} instead of {}",
            admitted.component,
            package.component
        );
        let mut expected = package
            .operations
            .iter()
            .map(|(_, token)| sealed(token))
            .collect::<BTreeSet<_>>();
        if package.id == OVERLAY_PACKAGE_ID {
            expected.insert((*OVERLAY_RECEIPT_PARTICIPANT).to_owned());
        }
        let observed = admitted.operations.keys().cloned().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            observed == expected,
            "{} component admitted the wrong operation set: {observed:?}",
            package.id
        );
        for token in expected {
            let fact = admitted
                .operations
                .get(&token)
                .with_context(|| format!("{} operation fact missing for {token}", package.id))?;
            let registered = fact.registered_operation.as_deref();
            if token == sealed("client-acme-receiving:quality/create-inspection") {
                anyhow::ensure!(
                    registered.is_none(),
                    "private operation {token} fabricated an authorization identity"
                );
            } else {
                anyhow::ensure!(
                    registered == Some(token.as_str()),
                    "operation {token} admitted a different authorization identity"
                );
            }
        }
        anyhow::ensure!(
            !admitted.effects.is_empty(),
            "{} component is not effectful",
            package.id
        );
        anyhow::ensure!(
            digests
                .insert(package.id.to_owned(), admitted.component_digest.clone())
                .is_none(),
            "{} admitted more than one component digest",
            package.id
        );
    }
    Ok(digests)
}

pub(super) fn gate_document(
    command_id: &str,
    package: JourneyPackage,
    document: &Value,
) -> anyhow::Result<wamn_authoring_model::AuthoringDocument> {
    wamn_test_infrastructure::declarations::gate_document(
        &wamn_test_infrastructure::declarations::GateInput {
            command_id: command_id.to_owned(),
            package: PackageCoordinate::new(package.id, package.version())?,
            scope: wamn_authoring_model::AuthoringScope {
                project_id: identity().project.clone(),
                environment: identity().environment.clone(),
            },
        },
        &serde_json::to_string(document)?,
    )
}

pub(super) async fn gate_journey_wirings(bind: &str, bearer: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::new();
    let mut reports = Vec::with_capacity(
        released_journey_packages()
            .map(|package| package.wirings.len())
            .sum(),
    );
    for package in released_journey_packages() {
        for wiring in package.wirings {
            let path = journey_publication_root(package)
                .join("wirings")
                .join(format!("{wiring}.json"));
            // The gate judges sealed ids, so the authored references resolve first.
            let document = wamn_schema_generator::operation_reference::read_authored_document(
                &path,
                wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
            )
            .with_context(|| format!("read {}", path.display()))?;
            let response = client
                .post(format!("http://{bind}/authoring"))
                .bearer_auth(bearer)
                .json(&gate_document(
                    &format!("gate-{}-{wiring}", package.id),
                    package,
                    &document,
                )?)
                .send()
                .await
                .with_context(|| {
                    format!("submit {}::{wiring} to the production Gate", package.id)
                })?;
            let status = response.status();
            let body: Value = response
                .json()
                .await
                .with_context(|| format!("decode {}::{wiring} Gate response", package.id))?;
            anyhow::ensure!(
                status == reqwest::StatusCode::OK
                    && body["body"]["outcome"]["status"] == "completed",
                "production Gate refused {}::{wiring}: status={status} body={body}",
                package.id
            );
            reports.push(
                body["body"]["outcome"]["value"]["result"]["report-id"]
                    .as_str()
                    .with_context(|| {
                        format!(
                            "production Gate returned no report id for {}::{wiring}",
                            package.id
                        )
                    })?
                    .to_owned(),
            );
        }
    }
    Ok(reports)
}

pub(super) async fn verify_zero_case_gate_reports(
    control: &Client,
    report_ids: &[String],
) -> anyhow::Result<()> {
    let expected_count = released_journey_packages()
        .map(|package| package.wirings.len())
        .sum::<usize>();
    anyhow::ensure!(
        report_ids.len() == expected_count,
        "production Gate returned {} reports for {expected_count} wirings",
        report_ids.len()
    );
    for report_id in report_ids {
        let row = control
            .query_one(
                "SELECT passed, summary FROM wamn_run.gate_reports \
                 WHERE tenant_id = $1 AND wiring_hash = $2",
                &[&identity().tenant.as_str(), report_id],
            )
            .await
            .with_context(|| format!("read production Gate report {report_id}"))?;
        let passed: bool = row.get(0);
        let summary: Value = row.get(1);
        anyhow::ensure!(
            passed && summary == serde_json::json!({"cases": 0}),
            "production Gate report {report_id} was not an accepted zero-case judgment: {summary}"
        );
    }
    Ok(())
}

pub(super) async fn author_journey_wirings(
    inputs: &JourneyDocument,
    project_url: &str,
    system_url: &str,
) -> anyhow::Result<()> {
    let registry = journey_package_registry(inputs)?;
    for package in released_journey_packages() {
        for wiring in package.wirings {
            let document = author_wiring::read_wiring_document(
                &journey_publication_root(package)
                    .join("wirings")
                    .join(format!("{wiring}.json")),
            )
            .with_context(|| format!("author gated wiring {}::{wiring}", package.id))?;
            author_wiring::author_wiring_in_databases(
                project_url,
                system_url,
                &registry,
                identity().tenant.as_str(),
                package.id,
                package.version(),
                &document,
            )
            .await
            .with_context(|| format!("author gated wiring {}::{wiring}", package.id))?;
        }
    }
    Ok(())
}

pub(super) struct JourneyReleaseTarget<'a> {
    pub(super) project_url: &'a str,
    pub(super) system_url: &'a str,
    pub(super) publisher: &'a str,
}

/// The package artifact registry of the journey: `<registry>/<org>/packages`
/// beside the component artifact base.
pub(super) fn journey_package_registry(
    inputs: &JourneyDocument,
) -> anyhow::Result<wamn_control::package_artifact::PackageRegistry> {
    let (parent, _) = inputs
        .component_artifact_base
        .rsplit_once('/')
        .context("the component artifact base names a repository")?;
    Ok(wamn_control::package_artifact::PackageRegistry {
        artifact_base: format!("{parent}/packages"),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
    })
}

/// Push the verified package artifact of every released journey package and
/// return each root with the digest the registry records, the one
/// apply-package installs (docs/plan/platform-deploy.md R3). The gate,
/// authorship and publish read component facts from these artifacts.
pub(super) async fn push_journey_packages(
    inputs: &JourneyDocument,
) -> anyhow::Result<Vec<wamn_control::apply_package::PackageRoot>> {
    let registry = journey_package_registry(inputs)?;
    let mut packages = Vec::new();
    for package in released_journey_packages() {
        let path = journey_package_root(package);
        let pushed = wamn_control::package_artifact::push_package(
            &wamn_control::package_artifact::PushPackageRequest {
                package: path.clone(),
                registry: registry.clone(),
                component_artifact_base: inputs.component_artifact_base.clone(),
            },
        )
        .await
        .with_context(|| format!("push the {} package artifact", package.id))?;
        packages.push(wamn_control::apply_package::PackageRoot {
            path,
            artifact_digest: Some(pushed.digest),
        });
    }
    Ok(packages)
}

pub(super) async fn publish_journey_release(
    inputs: &JourneyDocument,
    target: JourneyReleaseTarget<'_>,
) -> anyhow::Result<String> {
    let JourneyReleaseTarget {
        project_url,
        system_url,
        publisher,
    } = target;
    let wirings = released_journey_packages()
        .flat_map(|package| {
            package.wirings.iter().map(move |wiring| {
                format!("{}@{}::{wiring}=1", package.id, package.version())
                    .parse::<ReleaseWiringTarget>()
                    .map_err(anyhow::Error::msg)
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    // The release composes the verified package artifacts, which live beside
    // the component artifacts.
    let registry = journey_package_registry(inputs)?;
    let digest = publish_release::publish_release(
        PublishReleaseRequest {
            database_url: project_url.to_owned(),
            control_database_url: system_url.to_owned(),
            org: identity().org.clone(),
            tenant: identity().tenant.clone(),
            environment: identity().environment.clone(),
            verified_publisher_principal: publisher.to_owned(),
            run_schema: "wamn_run".to_owned(),
            packages: released_journey_packages()
                .map(|package| PackageCoordinate::new(package.id, package.version()))
                .collect::<Result<Vec<_>, _>>()?,
            wirings,
        },
        &registry,
        &inputs.release_artifact_base,
    )
    .await
    .context("publish the production Receiving release")?
    .to_string();
    // Publish writes OCI only. The journey installs the release as apply's
    // stage does: the row from the pushed manifest (R1 (3)).
    let bytes = ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )
    .context("configure the release puller")?
    .pull_verified(&digest)
    .await
    .context("pull the exact released manifest")?;
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    wamn_control::environment::stage::write_release(
        project_url,
        identity().tenant.as_str(),
        &digest,
        &manifest,
    )
    .await
    .context("install the Receiving release row")?;
    Ok(digest)
}

pub(super) async fn seed_receiving_business_rows(project: &Client) -> anyhow::Result<()> {
    bind_fixture_principal(project, identity().tenant.as_str()).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000201', 'DOCK-1'); \
             INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000401', 'SUPPLIER-401'), \
               ('00000000-0000-0000-0000-000000000402', 'SUPPLIER-402'), \
               ('00000000-0000-0000-0000-000000000403', 'SUPPLIER-403') \
             ON CONFLICT ON CONSTRAINT supplier_id_pkey DO NOTHING; \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version) \
             VALUES \
               ('00000000-0000-0000-0000-000000000301', 'PO-301', \
                '00000000-0000-0000-0000-000000000401', 'open', 1), \
               ('00000000-0000-0000-0000-000000000302', 'PO-302', \
                '00000000-0000-0000-0000-000000000402', 'open', 1); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000501', \
                '00000000-0000-0000-0000-000000000301', 1, \
                '00000000-0000-0000-0000-000000000101', 5.0000, 0.0000), \
               ('00000000-0000-0000-0000-000000000502', \
                '00000000-0000-0000-0000-000000000302', 1, \
                '00000000-0000-0000-0000-000000000101', 7.0000, 0.0000); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, \
                acme_inspection_required, acme_quality_status) \
             VALUES \
               ('00000000-0000-0000-0000-000000000303', 'PO-303', \
                '00000000-0000-0000-0000-000000000403', 'complete', 2, \
                true, 'pending');",
        )
        .await
        .context("seed only Receiving business rows")
}

pub(super) async fn seed_materializer_trigger_rows(project: &Client) -> anyhow::Result<()> {
    seed_materializer_order(
        project,
        "00000000-0000-0000-0000-000000000304",
        "00000000-0000-0000-0000-000000000504",
        "PO-304",
    )
    .await
}

/// One untouched open purchase order with one line, for one materializer
/// trigger. A rerun of the materializer stage seeds a new one.
pub(super) async fn seed_materializer_order(
    project: &Client,
    order_id: &str,
    line_id: &str,
    number: &str,
) -> anyhow::Result<()> {
    bind_fixture_principal(project, identity().tenant.as_str()).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000404', 'SUPPLIER-404') \
             ON CONFLICT ON CONSTRAINT supplier_id_pkey DO NOTHING;",
        )
        .await
        .context("seed the materializer journey supplier")?;
    project
        .execute(
            "INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version) \
             VALUES ($1::text::uuid, $2, '00000000-0000-0000-0000-000000000404', 'open', 1)",
            &[&order_id, &number],
        )
        .await
        .context("seed the untouched materializer journey purchase order")?;
    project
        .execute(
            "INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES ($1::text::uuid, $2::text::uuid, 1, \
                     '00000000-0000-0000-0000-000000000101', 9.0000, 0.0000)",
            &[&line_id, &order_id],
        )
        .await
        .context("seed the untouched materializer journey purchase order line")?;
    Ok(())
}
