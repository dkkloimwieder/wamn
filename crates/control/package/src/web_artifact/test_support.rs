//! Shared web artifact fixtures for control tests.

use super::{WEB_MANIFEST_FILE, web_manifest};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime};
use wamn_execution_contract::{bytes_sha256, canonical_json_bytes};
use wamn_package_artifact::PackageRegistry;
use wamn_package_artifact::registry_pull::test_server::Server;
use wamn_schema_generator::build_record::{
    self, BuildGenerator, BuildInputs, BuildOutputs, BuildPackage, BuildRecord,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

pub fn build_fixture(client: Option<&str>) -> (BuildRecord, Vec<u8>) {
    let mut manifest = serde_json::json!({
        "package": {"id": "web_fixture", "version": "1.0.0"},
        "required_platform_policy_contract": {"id": "web_fixture_data_access", "state": "satisfied"},
        "models": {}, "components": {}, "connections": {},
    });
    if let Some(client) = client {
        manifest["client_package"] = serde_json::json!({"name": client});
    }
    let compiled = canonical_json_bytes(&manifest);
    let record = BuildRecord {
        schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
        package: BuildPackage {
            id: "web_fixture".into(),
            version: "1.0.0".into(),
        },
        inputs: BuildInputs {
            digest: bytes_sha256(b"package inputs"),
            generation_digest: bytes_sha256(b"generation inputs"),
            files: BTreeMap::new(),
        },
        dependencies: Vec::new(),
        generator: BuildGenerator {
            id: "fixture".into(),
            toolchain: "fixture".into(),
            sqlx: "0.9.0".into(),
        },
        outputs: BuildOutputs {
            manifest: bytes_sha256(&compiled),
            files: BTreeMap::new(),
            components: Vec::new(),
        },
    };
    (record, compiled)
}

pub fn artifact_fixture(root: &Path, reversed: bool) {
    fs::create_dir_all(root.join("dist/assets")).unwrap();
    let mut files = vec![
        ("dist/index.html", b"index".as_slice()),
        ("dist/assets/app.js", b"script"),
    ];
    if reversed {
        files.reverse();
    }
    for (name, bytes) in files {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                SystemTime::UNIX_EPOCH + Duration::from_secs(if reversed { 900 } else { 100 }),
            ))
            .unwrap();
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if reversed { 0o755 } else { 0o600 }),
        )
        .unwrap();
    }
    let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
    fs::write(
        root.join(WEB_MANIFEST_FILE),
        web_manifest(&record, &compiled, "0.2.0".into())
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
}

pub fn fixture_registry(scratch: &ScratchRoot, server: &Server) -> PackageRegistry {
    let auth = scratch.path().join("registry-auth.json");
    fs::write(
        &auth,
        serde_json::to_vec(&serde_json::json!({"auths": {server.reference.registry(): {
            "username": "fixture", "password": "fixture"
        }}}))
        .unwrap(),
    )
    .unwrap();
    PackageRegistry {
        artifact_base: format!("{}/web", server.reference.registry()),
        registry_auth_file: auth,
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
    }
}
