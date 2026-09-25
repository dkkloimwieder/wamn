//! A release bundle loaded from a directory, and every refusal of one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ArtifactHash, ComponentPackageScope,
    EffectiveReleaseId, PackageCoordinate, RELEASE_MANIFEST_FILE_NAME, ServingComponent,
    ServingComponentOperation, ServingManifest, ServingRelease,
};
use wamn_edge::grants::GRANTS_FILE_NAME;
use wamn_edge::release::{
    BUNDLE_FILE_NAME, COMPONENTS_FILE_NAME, EdgeRelease, EdgeReleaseErrorKind, file_digest,
};

const TENANT: &str = "t1";
const PACKAGE: &str = "scale";
const VERSION: &str = "1.0.0";
const COMPONENT: &str = "device";
const PERMISSION: &str = "scale:device/record@1.0.0";
/// A registered export is keyed by its permission identity.
const OPERATION: &str = PERMISSION;
const BODY: &[u8] = b"the device component bytes";

fn manifest() -> ServingManifest {
    ServingManifest::new(
        ServingRelease {
            tenant_id: TENANT.into(),
            effective_release_id: EffectiveReleaseId::new(7).expect("release id"),
            environment: "edge".into(),
            packages: BTreeSet::from([PackageCoordinate::new(PACKAGE, VERSION).expect("package")]),
        },
        BTreeSet::from([ServingComponent {
            package_id: PACKAGE.into(),
            component: COMPONENT.into(),
            interface_version: "0.1".into(),
            digest: ArtifactHash::parse(file_digest(BODY)).expect("digest"),
            operations: BTreeMap::from([(
                OPERATION.into(),
                ServingComponentOperation {
                    pre_commit: None,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: Some(PERMISSION.into()),
                    permissions: BTreeSet::from([PERMISSION.into()]),
                    participant: None,
                    statements: BTreeMap::new(),
                },
            )]),
        }]),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("the fixture manifest is valid")
}

fn fact() -> AdmittedComponent {
    AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: TENANT.into(),
            package_id: PACKAGE.into(),
            package_version: VERSION.into(),
        },
        component: COMPONENT.into(),
        interface_version: "0.1".into(),
        operations: BTreeMap::from([(
            OPERATION.into(),
            AdmittedComponentOperation {
                pre_commit: None,
                pre_commit_required: false,
                registered_operation: Some(PERMISSION.into()),
                fresh_only: false,
                committed_result_schema: None,
                dependencies: Vec::new(),
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: file_digest(BODY),
        imports: Vec::new(),
        imports_fingerprint: String::new(),
        effects: Vec::new(),
    }
}

/// The files of one bundle, written as given, with a bundle that pins them.
struct Bundle {
    manifest: Vec<u8>,
    components: Vec<u8>,
    grants: Vec<u8>,
    body: Vec<u8>,
}

impl Bundle {
    fn valid() -> Self {
        Self {
            manifest: manifest().canonical_bytes(),
            components: serde_json::to_vec(&[fact()]).expect("facts"),
            grants: serde_json::to_vec(&json!({"roles": {"operator": [PERMISSION]}}))
                .expect("grants"),
            body: BODY.to_vec(),
        }
    }

    /// Write the files into a new directory and return it with the bundle digest.
    fn write(&self) -> (PathBuf, String) {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "edge-release-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).expect("create the bundle directory");
        let bundle = serde_json::to_vec(&json!({
            "format": 1,
            "manifest": file_digest(&self.manifest),
            "components": file_digest(&self.components),
            "grants": file_digest(&self.grants),
        }))
        .expect("bundle");
        write(&directory, RELEASE_MANIFEST_FILE_NAME, &self.manifest);
        write(&directory, COMPONENTS_FILE_NAME, &self.components);
        write(&directory, GRANTS_FILE_NAME, &self.grants);
        let tag = file_digest(BODY);
        let tag = tag.strip_prefix("sha256:").expect("digest prefix");
        write(&directory, &format!("{tag}.wasm"), &self.body);
        write(&directory, BUNDLE_FILE_NAME, &bundle);
        (directory, file_digest(&bundle))
    }
}

fn write(directory: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(directory.join(name), bytes).expect("write a bundle file");
}

async fn refusal(directory: &Path, digest: &str) -> EdgeReleaseErrorKind {
    EdgeRelease::load(directory, digest)
        .await
        .expect_err("the bundle is refused")
        .kind()
}

#[tokio::test]
async fn a_pinned_bundle_loads_and_grants_permissions_by_role() {
    let (directory, digest) = Bundle::valid().write();
    let release = EdgeRelease::load(&directory, &digest).await.expect("load");
    assert_eq!(release.bundle_digest(), digest);
    assert_eq!(release.release().release().effective_release_id, 7);
    assert_eq!(release.components(), [fact()]);
    assert_eq!(
        release
            .grants()
            .permissions(&["operator".into(), "viewer".into()]),
        BTreeSet::from([PERMISSION.to_owned()])
    );
    assert!(release.grants().permissions(&["viewer".into()]).is_empty());
}

#[tokio::test]
async fn a_changed_file_does_not_match_its_pin() {
    let (directory, digest) = Bundle::valid().write();
    assert_eq!(
        refusal(&directory, &file_digest(b"another bundle")).await,
        EdgeReleaseErrorKind::Mismatch
    );
    for name in [
        RELEASE_MANIFEST_FILE_NAME,
        COMPONENTS_FILE_NAME,
        GRANTS_FILE_NAME,
    ] {
        let (directory, digest) = Bundle::valid().write();
        let mut bytes = std::fs::read(directory.join(name)).expect("read");
        bytes.push(b' ');
        write(&directory, name, &bytes);
        assert_eq!(
            refusal(&directory, &digest).await,
            EdgeReleaseErrorKind::Mismatch,
            "{name}"
        );
    }
    let tag = file_digest(BODY);
    let tag = tag.strip_prefix("sha256:").expect("digest prefix");
    write(&directory, &format!("{tag}.wasm"), b"other component bytes");
    assert_eq!(
        refusal(&directory, &digest).await,
        EdgeReleaseErrorKind::Mismatch
    );
}

#[tokio::test]
async fn component_facts_must_match_the_manifest_one_to_one() {
    let missing = Bundle {
        components: serde_json::to_vec(&Vec::<AdmittedComponent>::new()).expect("facts"),
        ..Bundle::valid()
    };
    let repeated = Bundle {
        components: serde_json::to_vec(&[fact(), fact()]).expect("facts"),
        ..Bundle::valid()
    };
    let mut other = fact();
    other.component = "another".into();
    let foreign = Bundle {
        components: serde_json::to_vec(&[fact(), other]).expect("facts"),
        ..Bundle::valid()
    };
    for bundle in [missing, repeated, foreign] {
        let (directory, digest) = bundle.write();
        assert_eq!(
            refusal(&directory, &digest).await,
            EdgeReleaseErrorKind::Rejected
        );
    }
}

#[tokio::test]
async fn grants_name_only_permissions_the_release_requires() {
    for grants in [
        json!({"roles": {"operator": ["scale:device/other@1.0.0"]}}),
        json!({"roles": {"Operator": [PERMISSION]}}),
        json!({"roles": {"operator": [PERMISSION]}, "revoked": []}),
    ] {
        let bundle = Bundle {
            grants: serde_json::to_vec(&grants).expect("grants"),
            ..Bundle::valid()
        };
        let (directory, digest) = bundle.write();
        assert_eq!(
            refusal(&directory, &digest).await,
            EdgeReleaseErrorKind::Rejected,
            "{grants}"
        );
    }
}
