//! Receiving environment tests and helpers.

use super::*;


pub(super) fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving")
}

pub(super) fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
}

pub(super) fn journey_package_root(package: JourneyPackage) -> PathBuf {
    let source = if package.id == BASE_PACKAGE_ID {
        package_root()
    } else {
        overlay_package_root()
    };
    if std::env::var_os(JOURNEY_DOCUMENT_ENV).is_some()
        && let Some(root) = JourneyDocument::required()
            .expect("the journey package source requires a valid input document")
            .fresh_only_packages
    {
        return root.join(
            source
                .file_name()
                .expect("the proof package has a directory name"),
        );
    }
    if std::env::var_os(JOURNEY_DOCUMENT_ENV).is_some()
        && let Some(phase) = JourneyDocument::required()
            .expect("the compatibility proof requires a valid input document")
            .overlay_compatibility
    {
        return overlay_compatibility::source(&phase, package);
    }
    source
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

/// Resolve the scenario-worker binary the live gates spawn, refusing by name.
pub(super) fn journey_scenario_worker_binary() -> anyhow::Result<PathBuf> {
    let binary = required_journey_path(SCENARIO_WORKER_BIN_ENV)?;
    anyhow::ensure!(
        binary.is_file(),
        "{SCENARIO_WORKER_BIN_ENV} does not name a built wamn-scenario-worker binary: {}",
        binary.display()
    );
    Ok(binary)
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

pub(super) async fn install_journey_project(
    project: &Client,
    project_url: &str,
    fresh_only: bool,
) -> anyhow::Result<()> {
    install_journey_platform_floor(project).await?;
    for package in JOURNEY_PACKAGES {
        apply_package::run(ApplyPackageArgs {
            package: journey_package_root(package),
            database_url: project_url.to_owned(),
            tenant: TENANT.to_owned(),
        })
        .await
        .with_context(|| {
            format!(
                "apply {}@{} through the exact-byte runner",
                package.id, package.version
            )
        })?;
        if fresh_only && package.id == BASE_PACKAGE_ID {
            wamn_schema_generator::materialize_package_verified(
                wamn_schema_generator::MaterializeMode::Write,
                project_url,
                &journey_package_root(package),
            )
            .await
            .context("generate the copied fresh-only base before overlay migrations")?;
        }
    }
    Ok(())
}

pub(super) async fn reconcile_journey_data_access(project_url: &str) -> anyhow::Result<()> {
    let packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| journey_package_root(*package))
        .collect::<Vec<_>>();
    reconcile_package_data_access::reconcile_package_data_access(ReconcilePackageDataAccessArgs {
        packages: packages.clone(),
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("converge the fresh installed-set data-access union")?;
    let again = reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessArgs {
            packages,
            database_url: project_url.to_owned(),
            tenant: TENANT.to_owned(),
        },
    )
    .await
    .context("replay the installed-set data-access union")?;
    anyhow::ensure!(
        again.is_noop(),
        "installed-set data-access reconciliation did not converge"
    );
    Ok(())
}

pub(super) async fn verify_journey_operation_grants(project: &Client) -> anyhow::Result<()> {
    let observed = project
        .query(
            "SELECT permission FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 \
             ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read the installed two-package operation-grant union")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected = BASE_OPERATIONS
        .iter()
        .chain(OVERLAY_OPERATIONS.iter())
        .map(|(_, token)| (*token).to_owned())
        .filter(|token| token != "client-acme-receiving:quality/create-inspection@3.0.0")
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "installed packages projected the wrong route-caller grant union: {observed:?}"
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
    JOURNEY_PACKAGES
        .into_iter()
        .map(|package| {
            let source = journey_publication_root(package)
                .join("components")
                .join(format!("{}.json.in", package.component));
            // Like the disposable dev coordinator, layer this run's exact
            // virtualized bytes over authored pins without editing the package.
            let package_root = journey_package_root(package);
            let mut base_digests = wamn_ctl::dev::coordinator::authored_base_digests(&package_root)
                .with_context(|| format!("read {} base pins", package_root.display()))?;
            for base in JOURNEY_PACKAGES {
                let coordinate = format!("{}@{}", base.id, base.version);
                if let Some(digest) = base_digests.get_mut(coordinate.as_str()) {
                    let artifact = component_directory.join(format!("{}.wasm", base.component));
                    let bytes = std::fs::read(&artifact)
                        .with_context(|| format!("read built base {}", artifact.display()))?;
                    *digest = wamn_runtime::component_admission::component_digest(&bytes)
                        .into_boxed_str();
                }
            }
            let declaration = wamn_ctl::dev::coordinator::render_declaration_document(
                &source,
                TENANT,
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
    let manifest = overlay_package_root().join("wamn.json");
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
            declaration["operations"][OVERLAY_RECORD_RECEIPT]["dependencies"],
            serde_json::json!([{
                "package": BASE_PACKAGE_ID,
                "version": BASE_PACKAGE_VERSION,
                "digest": wamn_runtime::component_admission::component_digest(bytes),
                "operation": BASE_RECORD_RECEIPT,
            }]),
        );
    }
    assert_eq!(std::fs::read(manifest)?, authored_manifest);
    assert_eq!(std::fs::read(template)?, authored_template);
    Ok(())
}

pub(super) async fn push_journey_components(
    inputs: &JourneyDocument,
    project_url: &str,
    system_url: &str,
    declarations: &[JourneyComponentDeclaration],
) -> anyhow::Result<()> {
    for declaration in declarations {
        let package = declaration.package;
        push_component::run(PushComponentArgs {
            package: journey_package_root(package),
            component_bytes: inputs
                .component_directory
                .join(format!("{}.wasm", package.component)),
            declaration: declaration.path.clone(),
            artifact_base: inputs.component_artifact_base.clone(),
            registry_auth_file: inputs.registry_auth_file.clone(),
            insecure_registry: true,
            admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
            project_database_url: project_url.to_owned(),
            control_database_url: system_url.to_owned(),
        })
        .await
        .with_context(|| {
            format!(
                "publish production component {}@{}::{}",
                package.id, package.version, package.component
            )
        })?;
    }
    Ok(())
}

pub(super) async fn verify_journey_components_are_effectful(
    project: &Client,
) -> anyhow::Result<HashMap<String, String>> {
    let mut digests = HashMap::new();
    for package in JOURNEY_PACKAGES {
        let rows = project
            .query(
                "SELECT component, operations, effects, component_digest \
                 FROM catalog.component_library \
                 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
                 ORDER BY component COLLATE \"C\"",
                &[&TENANT, &package.id, &package.version],
            )
            .await
            .with_context(|| format!("read the admitted {} effect projection", package.id))?;
        anyhow::ensure!(
            rows.len() == 1,
            "component publication projected {} {} rows instead of one",
            rows.len(),
            package.id
        );
        let row = &rows[0];
        let component: String = row.get(0);
        anyhow::ensure!(
            component == package.component,
            "component publication projected {component} instead of {}",
            package.component
        );
        let operations: Value = row.get(1);
        let operation_facts = operations
            .as_object()
            .with_context(|| format!("{} operations fact is not an object", package.id))?;
        let expected = package
            .operations
            .iter()
            .map(|(_, token)| *token)
            .collect::<BTreeSet<_>>();
        let observed = operation_facts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            observed == expected,
            "{} component projected the wrong operation set: {observed:?}",
            package.id
        );
        for token in expected {
            let fact = operation_facts
                .get(token)
                .with_context(|| format!("{} operation fact missing for {token}", package.id))?;
            let registered = fact["registered-operation"].as_str();
            if token == "client-acme-receiving:quality/create-inspection@3.0.0" {
                anyhow::ensure!(
                    registered.is_none(),
                    "private operation {token} fabricated an authorization identity"
                );
            } else {
                anyhow::ensure!(
                    registered == Some(token),
                    "operation {token} projected a different authorization identity"
                );
            }
        }
        let effects: Value = row.get(2);
        anyhow::ensure!(
            effects
                .as_array()
                .is_some_and(|effects| !effects.is_empty()),
            "{} component is not effectful: {effects}",
            package.id
        );
        let digest: String = row.get(3);
        anyhow::ensure!(
            digests.insert(package.id.to_owned(), digest).is_none(),
            "{} projected more than one component digest",
            package.id
        );
    }
    Ok(digests)
}

pub(super) fn gate_document(command_id: &str, package: JourneyPackage, document: Value) -> Value {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "gate",
                "input": {
                    "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                    "package-id": package.id,
                    "package-version": package.version,
                    "document": document,
                },
            },
        },
    })
}

pub(super) async fn gate_journey_wirings(bind: &str, bearer: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::new();
    let mut reports = Vec::with_capacity(
        JOURNEY_PACKAGES
            .iter()
            .map(|package| package.operations.len())
            .sum(),
    );
    for package in JOURNEY_PACKAGES {
        for (wiring, _) in package.operations {
            let path = journey_publication_root(package)
                .join("wirings")
                .join(format!("{wiring}.json"));
            let document: Value = serde_json::from_slice(
                &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))?;
            let response = client
                .post(format!("http://{bind}/authoring"))
                .bearer_auth(bearer)
                .json(&gate_document(
                    &format!("gate-{}-{wiring}", package.id),
                    package,
                    document,
                ))
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
    let expected_count = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.operations.len())
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
                &[&TENANT, report_id],
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

pub(super) async fn author_journey_wirings(project_url: &str, system_url: &str) -> anyhow::Result<()> {
    for package in JOURNEY_PACKAGES {
        for (wiring, _) in package.operations {
            author_wiring::run(AuthorWiringArgs {
                database_url: project_url.to_owned(),
                control_database_url: system_url.to_owned(),
                tenant: TENANT.to_owned(),
                package_id: package.id.to_owned(),
                package_version: package.version.to_owned(),
                wiring_document: journey_publication_root(package)
                    .join("wirings")
                    .join(format!("{wiring}.json")),
            })
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
    pub(super) project: &'a Client,
    pub(super) control: &'a Client,
    pub(super) release_id: u32,
    pub(super) attachments: Vec<PathBuf>,
}

pub(super) async fn publish_journey_release(
    inputs: &JourneyDocument,
    target: JourneyReleaseTarget<'_>,
) -> anyhow::Result<(String, Arc<ReleaseManifestWeld>)> {
    let JourneyReleaseTarget {
        project_url,
        system_url,
        publisher,
        project,
        control,
        release_id,
        attachments,
    } = target;
    let wirings = JOURNEY_PACKAGES
        .iter()
        .flat_map(|package| {
            package.operations.iter().map(move |(wiring, _)| {
                format!("{}@{}::{wiring}=1", package.id, package.version)
                    .parse::<ReleaseWiringTarget>()
                    .map_err(anyhow::Error::msg)
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    publish_release::run(PublishReleaseArgs {
        database_url: project_url.to_owned(),
        control_database_url: system_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: release_id,
        environment: ENVIRONMENT.to_owned(),
        verified_publisher_principal: publisher.to_owned(),
        run_schema: "wamn_run".to_owned(),
        packages: JOURNEY_PACKAGES
            .iter()
            .map(|package| PackageCoordinate::new(package.id, package.version))
            .collect::<Result<Vec<_>, _>>()?,
        wirings,
        attachments,
        route_host: Some(inputs.route_host.clone()),
        package_manifests: JOURNEY_PACKAGES
            .iter()
            .map(|package| journey_package_root(*package).join("wamn.json"))
            .collect(),
    })
    .await
    .context("mint the production Receiving release")?;
    let inactive = control
        .query_opt(
            "SELECT deployed_manifest_hash FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(release_id as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("verify the minted Receiving release remains inactive")?;
    anyhow::ensure!(
        inactive.is_none(),
        "minting the Receiving release activated it before deployment"
    );
    let digest: String = project
        .query_one(
            "SELECT manifest_digest FROM catalog.release_manifest_v3_snapshots \
             WHERE tenant_id = $1 AND effective_release_id = $2",
            &[&TENANT, &(release_id as i32)],
        )
        .await
        .context("read the production-minted release digest")?
        .get(0);
    push_release_manifest::run(PushReleaseManifestArgs {
        database_url: project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: release_id,
        artifact_base: inputs.release_artifact_base.clone(),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
        control_database_url: system_url.to_owned(),
    })
    .await
    .context("push and attest the production Receiving release")?;
    let serving: String = control
        .query_one(
            "SELECT deployed_manifest_hash FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(release_id as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("verify the deployed Receiving release is serving")?
        .get(0);
    anyhow::ensure!(
        serving == digest,
        "serving attestation {serving} differs from minted release {digest}"
    );
    let source = ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )
    .context("configure the release puller")?;
    let bytes = source
        .pull_verified(&digest)
        .await
        .context("pull the exact released manifest")?;
    let origin = format!("{}@{digest}", inputs.release_artifact_base);
    let release = Arc::new(
        ReleaseManifestWeld::load_canonical_bytes(&bytes, &origin)
            .context("weld the pulled Receiving release")?,
    );
    Ok((digest, release))
}

pub(super) fn released_component_digests(
    release: &ReleaseManifestWeld,
    route_host: &str,
) -> anyhow::Result<HashMap<String, String>> {
    let expected_packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| PackageCoordinate::new(package.id, package.version))
        .collect::<Result<BTreeSet<_>, _>>()?;
    anyhow::ensure!(
        release.manifest().release.packages == expected_packages,
        "released manifest carries the wrong exact package membership"
    );
    let digests = release
        .manifest()
        .components
        .iter()
        .map(|component| {
            (
                component.package_id.clone(),
                component.digest.as_str().to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    let expected = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.id)
        .collect::<BTreeSet<_>>();
    let observed = digests.keys().map(String::as_str).collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "released manifest carries the wrong package component closure: {observed:?}"
    );
    anyhow::ensure!(
        release.manifest().components.len() == JOURNEY_PACKAGES.len()
            && release.manifest().components.iter().all(|component| {
                JOURNEY_PACKAGES.iter().any(|package| {
                    component.package_id == package.id && component.component == package.component
                })
            }),
        "released manifest does not carry exactly one component for each package"
    );
    let overlay = release
        .manifest()
        .components
        .iter()
        .find(|component| component.package_id == OVERLAY_PACKAGE_ID)
        .context("released manifest omitted the overlay component")?;
    let dependency = ComponentOperationDependency {
        package: BASE_PACKAGE_ID.to_owned(),
        version: BASE_PACKAGE_VERSION.to_owned(),
        digest: digests
            .get(BASE_PACKAGE_ID)
            .context("released manifest omitted the base component digest")?
            .clone(),
        operation: BASE_RECORD_RECEIPT.to_owned(),
    };
    anyhow::ensure!(
        overlay
            .operations
            .get(OVERLAY_RECORD_RECEIPT)
            .is_some_and(|operation| operation.dependencies == [dependency]),
        "released overlay record_receipt omitted its exact pinned dependency fact"
    );
    let expected_attachment_ids = JOURNEY_ATTACHMENTS
        .iter()
        .map(|attachment| attachment.id)
        .collect::<BTreeSet<_>>();
    let observed_attachment_ids = release
        .manifest()
        .attachments
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed_attachment_ids == expected_attachment_ids,
        "released manifest carries missing or extra route attachments: {observed_attachment_ids:?}"
    );
    for expected in JOURNEY_ATTACHMENTS {
        let attachment = &release.manifest().attachments[expected.id];
        anyhow::ensure!(
            attachment.kind == AttachmentKind::Http
                && attachment.package_id == expected.package_id
                && attachment.wiring_id == expected.wiring_id
                && attachment.wiring_version == 1
                && attachment.registered_operation.as_deref() == Some(expected.operation)
                && attachment.definition["route"]["method"] == "POST"
                && attachment.definition["route"]["path"] == expected.path
                && attachment.definition["route"]["host"] == route_host
                && attachment.auth_policy == serde_json::json!({"modes": ["pat"]}),
            "released attachment {} does not match its exact PAT route tuple: {attachment:?}",
            expected.id
        );
    }
    let registration = release
        .manifest()
        .registrations
        .get("client_acme_receiving::quality.create_inspection")
        .context("released manifest omitted the Acme receipt registration")?;
    anyhow::ensure!(
        registration.package_id == OVERLAY_PACKAGE_ID
            && registration.source_package_id == BASE_PACKAGE_ID
            && registration.entity == "receipt"
            && registration.ops == BTreeSet::from(["insert".to_owned()]),
        "released Acme receipt registration has the wrong owner/source/entity/ops: {registration:?}"
    );
    Ok(digests)
}

pub(super) async fn seed_receiving_business_rows(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000201', 'DOCK-1'); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000301', 'PO-301', \
                '00000000-0000-0000-0000-000000000401', 'open', 1, \
                '2026-08-31T12:00:00.000000Z', '2026-08-31T12:00:00.000000Z'), \
               ('00000000-0000-0000-0000-000000000302', 'PO-302', \
                '00000000-0000-0000-0000-000000000402', 'open', 1, \
                '2026-08-31T12:01:00.000000Z', '2026-08-31T12:01:00.000000Z'); \
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
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at, \
                acme_inspection_required, acme_quality_status) \
             VALUES \
               ('00000000-0000-0000-0000-000000000303', 'PO-303', \
                '00000000-0000-0000-0000-000000000403', 'complete', 2, \
                '2026-08-31T11:00:00.000000Z', '2026-08-31T11:30:00.000000Z', \
                true, 'pending');",
        )
        .await
        .context("seed only Receiving business rows")
}

// Distinct route-only approval precondition. This fixture does not claim that
// CDC or a materializer created the inspection; `.15.25.4` owns that proof.
pub(super) async fn seed_preexisting_quality_fixture(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.record_receipt_command \
               (idempotency_key, canonical_command, receipt_id, purchase_order_id, \
                purchase_order_status, row_version) \
             VALUES \
               ('quality-route-precondition', '\\x01', \
                '00000000-0000-0000-0000-000000000603', \
                '00000000-0000-0000-0000-000000000303', 'complete', 2); \
             INSERT INTO receiving.receipt \
               (id, idempotency_key, purchase_order_id, receipt_reference, occurred_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000603', 'quality-route-precondition', \
                '00000000-0000-0000-0000-000000000303', 'QUALITY-PREEXISTING', \
                '2026-08-31T11:30:00.000000Z'); \
             INSERT INTO receiving.quality_inspection (receipt_id, status, row_version) \
             VALUES ('00000000-0000-0000-0000-000000000603', 'pending', 1);",
        )
        .await
        .context("seed the distinct pre-existing quality approval fixture")
}

pub(super) async fn seed_materializer_trigger_rows(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000304', 'PO-304', \
                '00000000-0000-0000-0000-000000000404', 'open', 1, \
                '2026-08-31T12:03:00.000000Z', '2026-08-31T12:03:00.000000Z'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000504', \
                '00000000-0000-0000-0000-000000000304', 1, \
                '00000000-0000-0000-0000-000000000101', 9.0000, 0.0000);",
        )
        .await
        .context("seed the untouched materializer journey purchase order")
}
