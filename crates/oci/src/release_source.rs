//! Read built package documents for release assembly.
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use wamn_edge_document::ServingRoute;
use wamn_package::{ServingAttachment, canonical_name::CanonicalName};
use wamn_registry::ProjectEnvKey;
use wamn_release::component_admission::{ComponentAdmission, ComponentAdmissionEngine};
use wamn_release::package_source::LocalReleaseSource;

use crate::component_declaration::{authored_base_digests, render_package_declaration};
use crate::release_component::{AdmitComponentRequest, admit_component};
use wamn_release::publish_release::{
    AssembledRelease, PublishManifestError, PublishManifestErrorType, PublishReleaseRequest,
    RouteContracts, merge_package_attachment_documents, route_contracts,
};

type PackageManifestSources = (
    BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    RouteContracts,
);

/// Read the build's compiled routes for local edge assembly.
pub fn read_package_attachments(
    paths: &[PathBuf],
) -> Result<
    BTreeMap<wamn_package::canonical_name::CanonicalName, ServingAttachment>,
    PublishManifestError,
> {
    let documents = paths
        .iter()
        .map(|root| {
            let path = wamn_codegen::package_manifest_path(root);
            let read = || -> anyhow::Result<_> {
                let manifest =
                    wamn_component_policy::PackageManifest::from_slice(&std::fs::read(&path)?)?;
                Ok(manifest
                    .deploy
                    .ok_or_else(|| {
                        anyhow::anyhow!("build the package to compile its deploy section")
                    })?
                    .routes)
            };
            let routes = read().map_err(|error| {
                PublishManifestError::with_anyhow_source(
                    PublishManifestErrorType::Document,
                    format!("read routes from {}", path.display()),
                    error,
                )
            })?;
            Ok((path, routes))
        })
        .collect::<Result<Vec<_>, PublishManifestError>>()?;
    merge_package_attachment_documents(documents)
}

pub fn read_package_manifests(
    roots: &[PathBuf],
) -> Result<PackageManifestSources, PublishManifestError> {
    let mut manifests = BTreeMap::new();
    for root in roots {
        let path = wamn_codegen::package_manifest_path(root);
        let directory = crate::package_source::read_package_directory(root).map_err(|error| {
            PublishManifestError::with_anyhow_source(
                PublishManifestErrorType::PackageManifest,
                format!("read package directory {}", root.display()),
                error,
            )
        })?;
        let manifest = wamn_component_policy::PackageManifest::from_slice(
            &directory.manifest_bytes,
        )
        .map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("parse package manifest {}", path.display()),
                error,
            )
        })?;
        wamn_package::manifest::validate_operation_vocabulary(&manifest).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("validate package manifest {}", path.display()),
                error,
            )
        })?;
        let package_id = manifest.package.id.clone();
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
    }
    let types = route_contracts(&manifests)?;
    Ok((manifests, types))
}

/// Read built packages and assemble their local release.
pub fn assemble_release(
    args: &PublishReleaseRequest,
    roots: &[PathBuf],
    admissions: &[wamn_release::component_admission::ComponentAdmission],
    palette: &[wamn_release::palette::PaletteComponent],
) -> anyhow::Result<AssembledRelease> {
    let authored = read_package_attachments(roots)?;
    let (manifests, contracts) = read_package_manifests(roots)?;
    wamn_release::publish_release::assemble_release(
        args, &authored, &manifests, &contracts, admissions, palette,
    )
}

/// Read build artifacts for release assembly without publishing them.
pub struct LocalReleaseFiles<'a> {
    pub engine: &'a dyn ComponentAdmissionEngine,
}

impl fmt::Debug for LocalReleaseFiles<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalReleaseFiles")
            .finish_non_exhaustive()
    }
}

impl LocalReleaseSource for LocalReleaseFiles<'_> {
    fn admit(
        &self,
        package: &Path,
        component: &CanonicalName,
        bytes: &Path,
        target: &ProjectEnvKey,
        platform_packages: &[&str],
    ) -> anyhow::Result<ComponentAdmission> {
        let document = render_package_declaration(package, component, target)?;
        let declaration = TemporaryFile::write(&serde_json::to_vec(&document)?)?;
        admit_component(
            self.engine,
            AdmitComponentRequest {
                package: package.to_owned(),
                component_bytes: bytes.to_owned(),
                declaration: declaration.path().to_owned(),
                admitted_platform_packages: platform_packages
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                base_digests: authored_base_digests(package)?.into_values().collect(),
            },
        )
    }

    fn assemble(
        &self,
        request: &PublishReleaseRequest,
        package: &Path,
        admission: &ComponentAdmission,
    ) -> anyhow::Result<AssembledRelease> {
        assemble_release(
            request,
            &[package.to_owned()],
            std::slice::from_ref(admission),
            &[],
        )
    }
}

struct TemporaryFile(std::path::PathBuf);

impl TemporaryFile {
    fn write(bytes: &[u8]) -> anyhow::Result<Self> {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "wamn-edge-{}-{}.json",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let temporary = Self(path);
        file.write_all(bytes)?;
        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
/// The manifest route of one route attachment of the package at `root`, as
/// publish writes it from the generated contract of its operation.
///
/// A local application assembly calls it, so a test release carries the same
/// route facts as a published one. `roots` holds the directory of every
/// assembled package.
///
/// # Errors
///
/// Returns [`PublishManifestError`] when no generated contract declares the
/// operation, or a contract cannot be read.
fn package_route(
    roots: &BTreeMap<wamn_package::PackageName, &Path>,
    package_id: &wamn_package::PackageName,
    component: &wamn_package::canonical_name::CanonicalName,
    operation: &wamn_package::registered_operation::RegisteredOperation,
) -> Result<ServingRoute, PublishManifestError> {
    let unassembled = |package: &wamn_package::PackageName| {
        PublishManifestError::new(
            PublishManifestErrorType::PackageManifest,
            format!("package {package:?} is not assembled"),
        )
    };
    let root = roots
        .get(package_id)
        .ok_or_else(|| unassembled(package_id))?;
    let path = wamn_codegen::package_manifest_path(root);
    let bytes = std::fs::read(&path).map_err(|source| {
        PublishManifestError::with_source(
            PublishManifestErrorType::PackageManifest,
            format!("read {}", path.display()),
            source,
        )
    })?;
    let manifest =
        wamn_component_policy::PackageManifest::from_slice(&bytes).map_err(|source| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("parse {}", path.display()),
                source,
            )
        })?;
    wamn_release::publish_release::package_route(&manifest, component, operation)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use wamn_edge_document::ServingRelation;
    use wamn_package::OperationType;
    use wamn_test_infrastructure::operations::sealed;

    use super::BTreeMap;

    #[test]
    fn publication_reads_built_packages_with_and_without_source_migrations() {
        for with_migrations in [true, false] {
            let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
            let root = scratch.path().join("source_fixture");
            let output = wamn_codegen::output_root(&root);
            std::fs::create_dir_all(&output).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("wamn.k"),
                "package = {id = \"source_fixture\", version = \"1.0.0\"}\n",
            )
            .unwrap();
            let mut manifest: serde_json::Value =
                serde_json::from_str(wamn_test_infrastructure::publish::SOURCE_MANIFEST).unwrap();
            if with_migrations {
                std::fs::create_dir(output.join("migrations")).unwrap();
                std::fs::write(
                    output.join("migrations/0001_initial.sql"),
                    "CREATE TABLE inventory.item (id uuid);",
                )
                .unwrap();
            } else {
                manifest["models"]["item"]["owner"] = "base_fixture".into();
                manifest["models"]["item"]
                    .as_object_mut()
                    .unwrap()
                    .remove("audit_log");
                manifest["base_dependencies"] = serde_json::json!({
                    "base": {
                        "package": "base_fixture",
                        "version": "1.0.0",
                        "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "operations": ["item.get"],
                    }
                });
            }
            std::fs::write(
                output.join("wamn.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();

            super::read_package_manifests(&[root]).unwrap();
        }
    }

    fn app(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps")
            .join(name)
    }

    /// The route of one package, assembled alone.
    fn package_route(
        root: &Path,
        package_id: &wamn_package::PackageName,
        component: &wamn_package::canonical_name::CanonicalName,
        operation: &wamn_package::registered_operation::RegisteredOperation,
    ) -> Result<wamn_edge_document::ServingRoute, super::PublishManifestError> {
        let roots = BTreeMap::from([(package_id.to_owned(), root)]);
        super::package_route(&roots, package_id, component, operation)
    }

    fn relations(names: &[(&str, &str)]) -> BTreeSet<ServingRelation> {
        names
            .iter()
            .map(|(schema, relation)| ServingRelation {
                schema: (*schema).to_owned(),
                relation: (*relation).to_owned(),
            })
            .collect()
    }

    /// A route carries the relations its read reads and the revision field of
    /// its `get`, from the generated contract that publish reads.
    #[test]
    fn a_read_route_carries_its_relations_and_a_get_its_revision() {
        let wms = app("wamn_wms");
        let get = package_route(
            &wms,
            &"wamn_wms".parse().unwrap(),
            &"wms".parse().unwrap(),
            &sealed("wamn-wms:packaging/get"),
        )
        .expect("the packaging get has a contract");
        assert_eq!(get.type_, OperationType::Get);

        assert_eq!(get.reads, relations(&[("wms", "packaging")]));

        let query = package_route(
            &wms,
            &"wamn_wms".parse().unwrap(),
            &"wms".parse().unwrap(),
            &sealed("wamn-wms:packaging/query"),
        )
        .expect("the packaging query has a contract");
        assert_eq!((query.type_, query.revision), (OperationType::Query, None));
        assert_eq!(query.reads, relations(&[("wms", "packaging")]));

        let aggregate = package_route(
            &wms,
            &"wamn_wms".parse().unwrap(),
            &"wms".parse().unwrap(),
            &sealed("wamn-wms:inventory/aggregate"),
        )
        .expect("the authored projection has a contract");
        assert_eq!(
            aggregate.reads,
            relations(&[("wms", "packaging"), ("wms", "packaging_quantity")])
        );

        let history = package_route(
            &app("wamn_receiving"),
            &"wamn_receiving".parse().unwrap(),
            &"receiving".parse().unwrap(),
            &sealed("wamn-receiving:receiving/load-purchase-order-history"),
        )
        .expect("the history projection has a contract");
        assert_eq!(
            history.reads,
            relations(&[("receiving", "purchase_order")]),
            "a history table stands for its model relation"
        );

        let write = package_route(
            &wms,
            &"wamn_wms".parse().unwrap(),
            &"wms".parse().unwrap(),
            &sealed("wamn-wms:inventory/move"),
        )
        .expect("the move command has a contract");
        assert_eq!((write.reads, write.revision), (BTreeSet::new(), None));
    }

    /// A write route names the input field of its idempotency key, from the
    /// generated input contract. A read and a keyless write name none.
    #[test]
    fn a_write_route_names_its_idempotency_field() {
        let wms = app("wamn_wms");
        let route = |operation: &wamn_package::registered_operation::RegisteredOperation| {
            package_route(
                &wms,
                &"wamn_wms".parse().unwrap(),
                &"wms".parse().unwrap(),
                operation,
            )
            .expect("the operation has a contract")
            .idempotency
        };

        assert_eq!(route(&sealed("wamn-wms:packaging/get")), None);
        let archive = package_route(
            &app("platform_fixture"),
            &"platform_fixture".parse().unwrap(),
            &"widget".parse().unwrap(),
            &"platform-fixture:widget/archive@2.1.1".parse().unwrap(),
        );
        assert_eq!(
            archive.expect("the archive has a contract").idempotency,
            None
        );
    }
}
