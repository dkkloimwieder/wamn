//! Admit the file package's signed read, then carry its exact facts in a release.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::json;
use wamn_catalog::{AdmittedComponent, HostRouteSet, ServingComponent, ServingManifest};
use wamn_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_engine::artifact_source::local_component_path;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
use wamn_test_infrastructure::scratch::ScratchRoot;

pub(super) const OPERATION: &str = "file-fixture:document/signed-read@0.1.0";
pub(super) const ATTACHMENT: &str = "file-signed-read";
pub(super) const LIFETIME: u64 = 3;

pub(super) struct SignedReadFixture {
    pub(super) release: Arc<LoadedRelease>,
    pub(super) component: AdmittedComponent,
    pub(super) scratch: ScratchRoot,
}

pub(super) fn signed_read_fixture(env_key: &str) -> anyhow::Result<SignedReadFixture> {
    let scratch = ScratchRoot::create()?;
    let package = scratch.path().join("file_fixture");
    std::fs::create_dir_all(package.join("migrations"))?;
    std::fs::write(package.join("wamn.json"), super::files::PACKAGE)?;
    std::fs::write(
        package.join("migrations/0001_documents.sql"),
        super::files::DOCUMENTS,
    )?;
    // This normalized catalog is the fixture's two-column document table.
    // The live foreign-key tests apply and introspect its actual migration.
    let catalog = serde_json::from_value(json!({"tables": [{
        "schema": "file_fixture", "name": "document", "indexes": [],
        "columns": [
            {"name": "id", "type": "uuid", "nullable": false, "default": null, "generation": null},
            {"name": "file_id", "type": "uuid", "nullable": false, "default": null, "generation": null}
        ],
        "constraints": [
            {"name": "document_id_pkey", "type": "primary_key", "columns": ["id"]},
            {"name": "document_file_id_fkey", "type": "foreign_key",
             "columns": [{"column": "file_id", "referenced_column": "id"}],
             "referenced_schema": "platform", "referenced_table": "file",
             "on_update": "no_action", "on_delete": "no_action"}
        ]
    }]}))?;
    let statement = include_bytes!("query/read_file_id.sql");
    std::fs::create_dir_all(package.join("query"))?;
    std::fs::write(package.join("query/read_file_id.sql"), statement)?;
    let generated = wamn_schema_generator::generate(&wamn_schema_generator::GenerationInput::new(
        &catalog,
        super::files::PACKAGE.as_bytes(),
        &[wamn_schema_generator::AuthoredSql::new(
            "query/read_file_id.sql",
            statement,
        )],
        &wamn_schema_generator::StatementTransactionality::default(),
    ))?;
    for file in generated.files() {
        if file.path().starts_with("generated/") {
            let path = package.join(file.path());
            std::fs::create_dir_all(path.parent().context("generated file parent")?)?;
            std::fs::write(path, file.bytes())?;
        }
    }
    let declaration = wamn_control::component_declaration::render_palette_declaration(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../control/lib/tests/support/file_package/publication/components/signed-read.json.in",
        ),
        &wamn_catalog::ComponentPackageScope {
            env_key: env_key.into(),
            package_id: "file_fixture".into(),
            package_version: "0.1.0".into(),
        },
        None,
    )?;
    let declaration_path = package.join("signed-read.json");
    std::fs::write(&declaration_path, serde_json::to_vec(&declaration)?)?;
    let bytes = wat::parse_str(include_str!("signed-read.wat"))?;
    let wasm = package.join("signed-read.wasm");
    std::fs::write(&wasm, &bytes)?;
    let admitted = admit_component(AdmitComponentRequest {
        package: package.clone(),
        component_bytes: wasm,
        declaration: declaration_path,
        admitted_platform_packages: vec![
            "wamn:node".into(),
            "wamn:postgres".into(),
            "wasmcloud:blobstore".into(),
        ],
        base_digests: BTreeSet::new(),
    })?;
    let component = admitted.facts().clone();
    std::fs::write(
        local_component_path(scratch.path(), &component.component_digest)?,
        bytes,
    )?;
    let route = wamn_control::publish_release::package_route(
        &BTreeMap::from([("file_fixture".into(), package.as_path())]),
        "file_fixture",
        "signed_read",
        OPERATION,
    )?;
    let definition = json!({"id": ATTACHMENT, "type": "http", "route": {"path": "/files/signed-read"},
        "input-schema": {"type": "array"}, "raw-body-bytes": {"maximum": 1_048_576}});
    let manifest: ServingManifest = serde_json::from_value(json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": "file_fixture", "package-version": "0.1.0"}]},
        "components": [ServingComponent::project(&component, admitted.descriptor().connections)?],
        "routes": [route],
        "attachments": {ATTACHMENT: {
            "type": "http", "package-id": "file_fixture", "component": "signed_read", "operation": OPERATION,
            "registered-operation": OPERATION,
            "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
            "definition": definition, "auth-policy": {"modes": ["session"]}
        }},
        "host-routes": [HostRouteSet::Application]
    }))?;
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "signed read file fixture",
        ReleaseScope::new(env_key, "dev").with_applications(serde_json::from_value(json!({
            "files": {"packages": ["file_fixture"], "connections": {
                "files": super::files::STORE_NAME, "files_read": super::files::STORE_NAME,
                "files_no_cdn": "files-no-cdn"
            }}
        }))?),
    )?);
    Ok(SignedReadFixture {
        release,
        component,
        scratch,
    })
}
