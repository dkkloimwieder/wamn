//! Admission helpers shared by the integration and consumer publication cases.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::ComponentPackageScope;
use wamn_control::component_admission::{
    AdmitComponentRequest, ComponentAdmission, admit_component,
};
use wamn_control::component_declaration::{
    render_declaration_document, render_palette_declaration,
};

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

pub fn manifest(package: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(wamn_schema_generator::output_root(package).join("wamn.json")).unwrap(),
    )
    .unwrap()
}

pub fn admit(
    package: &Path,
    name: &str,
    palette: Option<&str>,
    bases: BTreeSet<String>,
) -> ComponentAdmission {
    let scope = ComponentPackageScope {
        tenant_id: "rest-publication".into(),
        package_id: manifest(package)["package"]["id"].as_str().unwrap().into(),
        package_version: "0.1.0".into(),
    };
    let declaration = match palette {
        Some(template) => {
            render_palette_declaration(&repository().join(template), &scope, None).unwrap()
        }
        None => render_declaration_document(
            &package.join(format!("publication/components/{name}.json.in")),
            &scope.tenant_id,
        )
        .unwrap(),
    };
    let rendered = std::env::temp_dir().join(format!(
        "rest-publication-{}-{name}-{}.json",
        scope.package_id,
        std::process::id()
    ));
    std::fs::write(&rendered, serde_json::to_vec(&declaration).unwrap()).unwrap();
    let admitted = admit_component(AdmitComponentRequest {
        package: package.into(),
        component_bytes: wamn_schema_generator::output_root(package)
            .join(format!("components/{name}.wasm")),
        declaration: rendered.clone(),
        admitted_platform_packages: vec![
            "wamn:node".into(),
            "wamn:postgres".into(),
            "wamn:connection".into(),
        ],
        base_digests: bases,
    });
    std::fs::remove_file(rendered).unwrap();
    admitted.unwrap_or_else(|error| panic!("admit {name}: {error:#}"))
}
