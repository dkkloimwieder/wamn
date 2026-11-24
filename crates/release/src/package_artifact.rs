//! Publication contracts for verified package and component artifacts.

use std::path::PathBuf;

use wamn_component_policy::ComponentDescriptor;
use wamn_digest::Digest;

use crate::component_admission::{ComponentAdmissionEngine, admit};
use std::collections::BTreeSet;
use wamn_component_policy::PackageManifest;
use wamn_package::ComponentDeclaration;

/// Publish a built source closure in dependency order.
#[async_trait::async_trait]
pub trait PackagePublisher: Send + Sync {
    async fn publish(&self, roots: Vec<PathBuf>) -> anyhow::Result<Digest>;
}

/// One admitted component and its original bytes.
#[derive(Debug)]
pub struct VerifiedComponent {
    descriptor: ComponentDescriptor,
    bytes: Box<[u8]>,
}

impl VerifiedComponent {
    pub fn descriptor(&self) -> &ComponentDescriptor {
        &self.descriptor
    }

    pub fn into_descriptor(self) -> ComponentDescriptor {
        self.descriptor
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Check exact build bytes against the package before publishing.
pub fn verify_component(
    engine: &dyn ComponentAdmissionEngine,
    manifest: &PackageManifest,
    bytes: Vec<u8>,
    declaration: ComponentDeclaration,
    admitted_platform_packages: BTreeSet<String>,
    base_digests: BTreeSet<Digest>,
) -> anyhow::Result<VerifiedComponent> {
    let (descriptor, _) = admit(
        engine,
        manifest,
        &bytes,
        declaration,
        admitted_platform_packages,
        base_digests,
    )?;
    Ok(VerifiedComponent {
        descriptor,
        bytes: bytes.into_boxed_slice(),
    })
}

/// A digest-addressed component and the registry transport inputs for its push.
pub struct ComponentPush<'a> {
    pub reference: &'a str,
    pub component: &'a VerifiedComponent,
    pub credentials: Option<(&'a str, &'a str)>,
    pub insecure: bool,
    pub ca_paths: &'a [PathBuf],
}

impl std::fmt::Debug for ComponentPush<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComponentPush")
            .field("reference", &self.reference)
            .field(
                "component_digest",
                &self.component.descriptor().component_digest,
            )
            .finish_non_exhaustive()
    }
}

/// Publish the stock component image for already admitted bytes.
#[async_trait::async_trait]
pub trait ComponentPublisher: Send + Sync {
    async fn push(&self, request: ComponentPush<'_>) -> anyhow::Result<Digest>;
}
