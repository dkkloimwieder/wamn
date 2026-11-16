//! Admission helpers shared by the integration and consumer publication cases.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::ComponentPackageCoordinate;
use wamn_publish::component_admission::{
    AdmitComponentRequest, ComponentAdmission, admit_component,
};
use wamn_publish::component_declaration::render_package_declaration;

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn manifest(package: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(wamn_codegen::output_root(package).join("wamn.json")).unwrap(),
    )
    .unwrap()
}

pub fn admit(
    package: &Path,
    name: &str,
    bases: BTreeSet<wamn_digest::Digest>,
) -> ComponentAdmission {
    let target = ComponentPackageCoordinate {
        project_env_key: "rest--test--publication".parse().unwrap(),
        package_id: manifest(package)["package"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
        package_version: "0.1.1".parse().unwrap(),
    };
    let declaration =
        render_package_declaration(package, &name.parse().unwrap(), &target.project_env_key)
            .unwrap();
    let rendered = std::env::temp_dir().join(format!(
        "rest-publication-{}-{name}-{}.json",
        target.package_id,
        std::process::id()
    ));
    std::fs::write(&rendered, serde_json::to_vec(&declaration).unwrap()).unwrap();
    let component_bytes =
        wamn_codegen::output_root(package).join(format!("components/{name}.wasm"));
    let admitted = admit_component(AdmitComponentRequest {
        package: package.into(),
        component_bytes,
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

/// Compile and seal the platform palette through the product builder.
pub fn palette() -> Vec<wamn_publish::palette::PaletteComponent> {
    let repository = repository();
    let components = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(wamn_build::palette::build(&repository))
    })
    .join()
    .expect("the palette compiler returns")
    .expect("build the platform palette");
    components
        .into_iter()
        .map(|component| wamn_publish::palette::PaletteComponent {
            facts: component
                .declaration
                .into_admitted(wamn_catalog::palette_target(
                    &"rest--test--publication".parse().unwrap(),
                )),
        })
        .collect()
}
