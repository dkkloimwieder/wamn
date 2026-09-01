//! The immutable release-serving manifest mounted by every serving process.
//!
//! Format 3 closes over exact package membership, component digests, wiring
//! definitions, attachments, and
//! registrations. It contains no flow, execution-plan, call-graph, or callable
//! identity. Producers must source every member from current catalog records;
//! this model intentionally provides no legacy-plan conversion.
//!
//! The document identity is the SHA-256 of its RFC 8785 canonical JSON. Sets and
//! maps make each collection's order deterministic, while
//! [`ServingManifest::from_canonical_bytes`] rejects bytes whose order or JSON
//! encoding differs from that canonical representation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ArtifactHash, AttachmentKind, CatalogIdentityError, DefinitionHash, EffectiveReleaseId,
    HASH_PREFIX, ManifestDigest, PackageCoordinate,
    package::validate_canonical_operation_for_package, validate_digest, validate_text,
};

/// The only serving-manifest format admitted by this revision.
pub const SERVING_MANIFEST_FORMAT_VERSION: u32 = 3;

/// The attachment auth-policy mode that permits an unauthenticated caller.
pub const NO_AUTHENTICATION_MODE: &str = "none";

/// The attachment auth-policy mode that requires a platform access token.
pub const PAT_AUTHENTICATION_MODE: &str = "pat";

/// Stable refusal literal for malformed or unsupported attachment auth policy.
pub const INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL: &str = "invalid-attachment-auth-policy";

/// Stable refusal literal for a serving-manifest format this reader will not admit.
pub const UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL: &str =
    "unsupported-serving-manifest-version";

/// Name prefix of the immutable, digest-named ConfigMap carrying the manifest.
pub const RELEASE_MANIFEST_CONFIGMAP_PREFIX: &str = "release-manifest-";

/// Directory the manifest ConfigMap is projected into on every pod.
pub const RELEASE_MANIFEST_MOUNT_PATH: &str = "/etc/wamn/release-manifest";

/// The manifest ConfigMap's single key and mounted file name.
pub const RELEASE_MANIFEST_FILE_NAME: &str = "manifest.json";

/// Byte ceiling shared by the manifest mint and mount reader.
pub const MAX_SERVING_MANIFEST_BYTES: usize = 1024 * 1024;

/// The name of the ConfigMap carrying the manifest with this digest.
pub fn release_manifest_configmap_name(
    manifest_digest: &str,
) -> Result<String, CatalogIdentityError> {
    validate_digest(manifest_digest, "manifest-digest")?;
    let hex = manifest_digest
        .strip_prefix(HASH_PREFIX)
        .expect("a validated digest carries the sha256 prefix");
    Ok(format!("{RELEASE_MANIFEST_CONFIGMAP_PREFIX}{hex}"))
}

/// The release coordinate and environment this manifest projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelease {
    pub tenant_id: String,
    pub effective_release_id: EffectiveReleaseId,
    pub environment: String,
    pub packages: BTreeSet<PackageCoordinate>,
}

/// One immutable component artifact in the release closure.
///
/// One operation exported by a release component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponentOperation {
    /// Explicit application permission identity. Palette exports carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
}

/// One immutable component artifact in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponent {
    pub package_id: String,
    pub component: String,
    pub interface_version: String,
    pub digest: ArtifactHash,
    pub operations: BTreeMap<String, ServingComponentOperation>,
}

/// One immutable wiring definition in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingWiring {
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub graph_hash: DefinitionHash,
}

/// One release attachment targeting an exact wiring identity and version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingAttachment {
    pub kind: AttachmentKind,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub definition_hash: DefinitionHash,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment. Attachments that
    /// do not invoke a package operation carry no token; callers never infer one
    /// from route, wiring, or component syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
}

/// The delivery grain frozen for one release registration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServingRegistrationInput {
    #[default]
    Event,
    Batch,
}

fn registration_input_is_event(input: &ServingRegistrationInput) -> bool {
    *input == ServingRegistrationInput::Event
}

/// One event registration targeting an exact wiring identity and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRegistration {
    /// Package that owns the target wiring and registration definition.
    pub package_id: String,
    /// Package whose committed change emits the event this registration reads.
    pub source_package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub entity: String,
    pub ops: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "registration_input_is_event")]
    pub input: ServingRegistrationInput,
}

/// The complete release document a serving process mounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingManifest {
    pub format_version: u32,
    pub release: ServingRelease,
    pub components: BTreeSet<ServingComponent>,
    pub wirings: BTreeSet<ServingWiring>,
    pub attachments: BTreeMap<String, ServingAttachment>,
    pub registrations: BTreeMap<String, ServingRegistration>,
}

impl ServingManifest {
    /// Build and validate a manifest from authoritative current-record facts.
    ///
    /// This helper is test-only. The production mint serializes its projected
    /// facts and admits those exact bytes through [`Self::from_canonical_bytes`].
    #[cfg(feature = "test-util")]
    pub fn new(
        release: ServingRelease,
        components: BTreeSet<ServingComponent>,
        wirings: BTreeSet<ServingWiring>,
        attachments: BTreeMap<String, ServingAttachment>,
        registrations: BTreeMap<String, ServingRegistration>,
    ) -> Result<Self, CatalogIdentityError> {
        let manifest = Self {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release,
            components,
            wirings,
            attachments,
            registrations,
        };
        manifest.validate()?;
        within_delivery_limit(manifest.canonical_bytes().len())?;
        Ok(manifest)
    }

    /// The RFC 8785 canonical bytes mounted by serving processes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        wamn_execution_contract::canonical_json_bytes(&self.as_value())
    }

    /// The content digest over [`Self::canonical_bytes`].
    pub fn digest(&self) -> ManifestDigest {
        ManifestDigest::parse(wamn_execution_contract::canonical_json_sha256(
            &self.as_value(),
        ))
        .expect("the shared canonicalizer emits a canonical sha256 digest")
    }

    /// Parse, validate, and admit only canonical format-3 bytes.
    ///
    /// The version is classified before the format-3 schema is decoded. This is
    /// what makes an older mount an explicit typed refusal rather than a
    /// generic unknown-field parse error, and it deliberately provides no
    /// dual-version tolerance.
    pub fn from_canonical_bytes(
        bytes: &[u8],
    ) -> Result<(Self, ManifestDigest), CatalogIdentityError> {
        within_delivery_limit(bytes.len())?;
        let document = serde_json::from_slice::<Value>(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        validate_format_version(&document)?;
        let manifest = serde_json::from_value::<Self>(document).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.canonical_bytes() != bytes {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let digest = manifest.digest();
        Ok((manifest, digest))
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("serving manifest serializes")
    }

    fn validate(&self) -> Result<(), CatalogIdentityError> {
        if self.format_version != SERVING_MANIFEST_FORMAT_VERSION {
            return Err(CatalogIdentityError::UnsupportedServingManifestVersion {
                requested: self.format_version.to_string(),
            });
        }
        validate_text(&self.release.tenant_id, "tenant-id")?;
        validate_text(&self.release.environment, "environment")?;
        if self.release.packages.is_empty() {
            return invalid("an effective release must contain at least one exact package pair");
        }
        let mut package_versions = BTreeMap::new();
        for package in &self.release.packages {
            if package_versions
                .insert(package.package_id(), package.package_version())
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateMember {
                    field: "release-packages",
                    id: package.package_id().to_owned(),
                });
            }
        }

        for component in &self.components {
            validate_package_member(&package_versions, &component.package_id)?;
            validate_text(&component.component, "component")?;
            validate_text(&component.interface_version, "interface-version")?;
            if component.operations.is_empty() {
                return invalid("a serving component must export at least one operation");
            }
            for (export, operation) in &component.operations {
                validate_text(export, "component-operation")?;
                validate_registered_operation(
                    &package_versions,
                    &component.package_id,
                    operation.registered_operation.as_deref(),
                )?;
                if operation
                    .registered_operation
                    .as_deref()
                    .is_some_and(|registered| registered != export)
                {
                    return invalid(format!(
                        "component export {export:?} and registered operation {:?} differ",
                        operation.registered_operation
                    ));
                }
            }
        }

        let mut targets = BTreeSet::new();
        for wiring in &self.wirings {
            validate_package_member(&package_versions, &wiring.package_id)?;
            validate_text(&wiring.wiring_id, "wiring-id")?;
            if wiring.wiring_version == 0 {
                return Err(CatalogIdentityError::ZeroVersion {
                    field: "wiring-version",
                });
            }
            if !targets.insert((
                wiring.package_id.as_str(),
                wiring.wiring_id.as_str(),
                wiring.wiring_version,
            )) {
                return invalid("a wiring identity-version pair occurs more than once");
            }
        }

        for (attachment_id, attachment) in &self.attachments {
            validate_text(attachment_id, "attachment-id")?;
            validate_package_member(&package_versions, &attachment.package_id)?;
            validate_wiring_target(
                &targets,
                &attachment.package_id,
                &attachment.wiring_id,
                attachment.wiring_version,
            )?;
            if !attachment.definition.is_object() || !attachment.auth_policy.is_object() {
                return invalid("attachment definition and resolved source must be JSON objects");
            }
            let auth_mode = attachment
                .auth_policy
                .as_object()
                .and_then(|policy| policy.get("mode"))
                .and_then(Value::as_str);
            if !matches!(
                auth_mode,
                Some(NO_AUTHENTICATION_MODE | PAT_AUTHENTICATION_MODE)
            ) {
                return Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                    attachment_id: attachment_id.clone(),
                });
            }
            if auth_mode == Some(NO_AUTHENTICATION_MODE)
                && attachment.registered_operation.is_some()
            {
                return Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
                    attachment_id: attachment_id.clone(),
                });
            }
            if contains_retired_identity(&attachment.definition)
                || contains_retired_identity(&attachment.auth_policy)
            {
                return invalid("attachment configuration carries retired flow or plan identity");
            }
            validate_registered_operation(
                &package_versions,
                &attachment.package_id,
                attachment.registered_operation.as_deref(),
            )?;
        }

        for (registration_id, registration) in &self.registrations {
            validate_package_member(&package_versions, &registration.package_id)?;
            validate_package_member(&package_versions, &registration.source_package_id)?;
            let (owner_package_id, local_registration_id) = registration_id
                .split_once("::")
                .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                    message: format!(
                        "registration key {registration_id:?} must be <package-id>::<registration-id>"
                    ),
                })?;
            validate_text(local_registration_id, "registration-id")?;
            if owner_package_id != registration.package_id || local_registration_id.contains("::") {
                return invalid(format!(
                    "registration key {registration_id:?} does not name owner package {:?}",
                    registration.package_id
                ));
            }
            validate_wiring_target(
                &targets,
                &registration.package_id,
                &registration.wiring_id,
                registration.wiring_version,
            )?;
            validate_text(&registration.entity, "entity")?;
            if registration.ops.is_empty() {
                return invalid("a registration matching no op is inert");
            }
            for op in &registration.ops {
                validate_text(op, "op")?;
            }
        }
        Ok(())
    }
}

fn validate_format_version(document: &Value) -> Result<(), CatalogIdentityError> {
    let Some(version) = document.get("format-version") else {
        return invalid("serving manifest format-version is required");
    };
    if version.as_u64() == Some(u64::from(SERVING_MANIFEST_FORMAT_VERSION)) {
        return Ok(());
    }
    let requested = match version {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    Err(CatalogIdentityError::UnsupportedServingManifestVersion { requested })
}

fn validate_wiring_target(
    targets: &BTreeSet<(&str, &str, u32)>,
    package_id: &str,
    wiring_id: &str,
    wiring_version: u32,
) -> Result<(), CatalogIdentityError> {
    validate_text(wiring_id, "wiring-id")?;
    if wiring_version == 0 {
        return Err(CatalogIdentityError::ZeroVersion {
            field: "wiring-version",
        });
    }
    if !targets.contains(&(package_id, wiring_id, wiring_version)) {
        return Err(CatalogIdentityError::UnresolvableManifestWiring {
            package_id: package_id.to_string(),
            wiring_id: wiring_id.to_string(),
            wiring_version,
        });
    }
    Ok(())
}

fn validate_package_member(
    package_versions: &BTreeMap<&str, &str>,
    package_id: &str,
) -> Result<(), CatalogIdentityError> {
    validate_text(package_id, "package-id")?;
    if !package_versions.contains_key(package_id) {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: format!("package {package_id:?} is absent from the effective release"),
        });
    }
    Ok(())
}

fn validate_registered_operation(
    package_versions: &BTreeMap<&str, &str>,
    package_id: &str,
    operation: Option<&str>,
) -> Result<(), CatalogIdentityError> {
    let Some(operation) = operation else {
        return Ok(());
    };
    let package_version = package_versions
        .get(package_id)
        .expect("package membership was validated before operation identity");
    validate_canonical_operation_for_package(operation, package_id, package_version)
}

fn contains_retired_identity(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "flow-id"
                    | "flow_id"
                    | "plan-hash"
                    | "plan_hash"
                    | "calls"
                    | "callable-contract"
                    | "source-artifact"
                    | "binding-base-artifact"
            ) || contains_retired_identity(value)
        }),
        Value::Array(values) => values.iter().any(contains_retired_identity),
        _ => false,
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    })
}

fn within_delivery_limit(bytes: usize) -> Result<(), CatalogIdentityError> {
    if bytes > MAX_SERVING_MANIFEST_BYTES {
        return Err(CatalogIdentityError::ManifestTooLarge {
            bytes,
            limit: MAX_SERVING_MANIFEST_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPONENT_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMPONENT_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GRAPH_A: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const GRAPH_B: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const DEFINITION: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn artifact_hash(value: &str) -> ArtifactHash {
        ArtifactHash::parse(value).expect("fixture artifact hash is canonical")
    }

    fn definition_hash(value: &str) -> DefinitionHash {
        DefinitionHash::parse(value).expect("fixture definition hash is canonical")
    }

    fn release() -> ServingRelease {
        ServingRelease {
            tenant_id: "t1".into(),
            effective_release_id: EffectiveReleaseId::new(7).unwrap(),
            environment: "prod".into(),
            packages: BTreeSet::from([
                PackageCoordinate::new("base", "1.0.0").unwrap(),
                PackageCoordinate::new("overlay", "3.0.0").unwrap(),
            ]),
        }
    }

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([
            ServingComponent {
                package_id: "overlay".into(),
                component: "transform".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_B),
                operations: BTreeMap::from([(
                    "map".into(),
                    ServingComponentOperation {
                        registered_operation: None,
                    },
                )]),
            },
            ServingComponent {
                package_id: "base".into(),
                component: "http-request".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_A),
                operations: BTreeMap::from([(
                    "base:purchase-order/get@1.0.0".into(),
                    ServingComponentOperation {
                        registered_operation: Some("base:purchase-order/get@1.0.0".into()),
                    },
                )]),
            },
        ])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([
            ServingWiring {
                package_id: "overlay".into(),
                wiring_id: "shipping".into(),
                wiring_version: 2,
                graph_hash: definition_hash(GRAPH_B),
            },
            ServingWiring {
                package_id: "base".into(),
                wiring_id: "orders".into(),
                wiring_version: 3,
                graph_hash: definition_hash(GRAPH_A),
            },
        ])
    }

    fn attachment() -> ServingAttachment {
        ServingAttachment {
            kind: AttachmentKind::Http,
            package_id: "base".into(),
            wiring_id: "orders".into(),
            wiring_version: 3,
            definition_hash: definition_hash(DEFINITION),
            definition: serde_json::json!({
                "id": "orders",
                "kind": "http",
                "route": {"host": "*", "path": "/orders", "method": "POST"}
            }),
            auth_policy: serde_json::json!({"mode": "pat"}),
            registered_operation: Some("base:purchase-order/get@1.0.0".into()),
        }
    }

    fn registration() -> ServingRegistration {
        ServingRegistration {
            package_id: "overlay".into(),
            source_package_id: "base".into(),
            wiring_id: "shipping".into(),
            wiring_version: 2,
            entity: "orders".into(),
            ops: BTreeSet::from(["insert".to_string()]),
            input: ServingRegistrationInput::Event,
        }
    }

    fn manifest() -> ServingManifest {
        ServingManifest::new(
            release(),
            components(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect("fixture manifest is valid")
    }

    #[test]
    fn collection_insertion_order_cannot_reach_identity() {
        let forward = manifest();
        let reversed = ServingManifest::new(
            release(),
            components().into_iter().rev().collect(),
            wirings().into_iter().rev().collect(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect("reordered fixture is valid");

        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
    }

    #[test]
    fn only_canonical_format_three_bytes_are_admitted() {
        let manifest = manifest();
        let bytes = manifest.canonical_bytes();
        assert_eq!(
            ServingManifest::from_canonical_bytes(&bytes),
            Ok((manifest.clone(), manifest.digest()))
        );

        let value: Value = serde_json::from_slice(&bytes).expect("canonical bytes are JSON");
        let indented = serde_json::to_vec_pretty(&value).expect("document serializes");
        assert_eq!(
            ServingManifest::from_canonical_bytes(&indented),
            Err(CatalogIdentityError::NonCanonicalJson)
        );
    }

    #[test]
    fn format_two_is_a_typed_refusal_not_a_compatibility_arm() {
        let legacy = br#"{"attachments":{},"components":[],"format-version":2,"registrations":{},"release":{},"wirings":[]}"#;
        let error = ServingManifest::from_canonical_bytes(legacy)
            .expect_err("format two must never enter the format-three decoder");
        assert_eq!(
            error,
            CatalogIdentityError::UnsupportedServingManifestVersion {
                requested: "2".into()
            }
        );
        assert!(
            error
                .to_string()
                .starts_with(UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL)
        );
    }

    #[test]
    fn attachment_operation_is_explicit_and_canonical() {
        let mut malformed = attachment();
        malformed.registered_operation = Some("purchase_order.get".into());
        let error = ServingManifest::new(
            release(),
            components(),
            wirings(),
            BTreeMap::from([("orders".to_string(), malformed)]),
            BTreeMap::new(),
        )
        .expect_err("an attachment cannot smuggle a local-only operation token");
        assert!(
            error
                .to_string()
                .contains("<package-id>:<module>/<action>@<package-version>")
        );
    }

    #[test]
    fn registered_operations_match_the_containing_package_coordinate() {
        for operation in [
            "overlay:purchase-order/get@3.0.0",
            "base:purchase-order/get@2.0.0",
        ] {
            let mut mismatched = attachment();
            mismatched.registered_operation = Some(operation.into());
            let error = ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::from([("orders".to_string(), mismatched)]),
                BTreeMap::new(),
            )
            .expect_err("attachment operation must match its selected package coordinate");
            assert!(error.to_string().contains("does not belong"));
        }

        let mut mismatched_components = components();
        let mut component = mismatched_components
            .pop_first()
            .expect("fixture has a component");
        let component_operation = component
            .operations
            .values_mut()
            .next()
            .expect("fixture component has an operation");
        component_operation.registered_operation = Some("overlay:purchase-order/get@3.0.0".into());
        mismatched_components.insert(component);
        let error = ServingManifest::new(
            release(),
            mismatched_components,
            wirings(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect_err("component operation must match its selected package coordinate");
        assert!(error.to_string().contains("does not belong"));
    }

    #[test]
    fn attachment_auth_policy_mode_is_closed_and_required() {
        for policy in [
            serde_json::json!({}),
            serde_json::json!({"mode": 7}),
            serde_json::json!({"mode": "invented"}),
        ] {
            let mut malformed = attachment();
            malformed.auth_policy = policy;
            assert_eq!(
                ServingManifest::new(
                    release(),
                    components(),
                    wirings(),
                    BTreeMap::from([("orders".to_string(), malformed)]),
                    BTreeMap::new(),
                ),
                Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                    attachment_id: "orders".into(),
                })
            );
        }

        let mut pat = attachment();
        pat.auth_policy = serde_json::json!({"mode": PAT_AUTHENTICATION_MODE});
        ServingManifest::new(
            release(),
            components(),
            wirings(),
            BTreeMap::from([("orders".to_string(), pat)]),
            BTreeMap::new(),
        )
        .expect("PAT is the other supported attachment auth mode");

        let mut anonymous = attachment();
        anonymous.auth_policy = serde_json::json!({"mode": NO_AUTHENTICATION_MODE});
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::from([("orders".to_string(), anonymous)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
                attachment_id: "orders".into(),
            })
        );
    }

    #[test]
    fn one_effective_release_selects_only_one_version_of_each_package() {
        let mut duplicate = release();
        duplicate
            .packages
            .insert(PackageCoordinate::new("base", "2.0.0").unwrap());
        let error = ServingManifest::new(
            duplicate,
            components(),
            wirings(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect_err("two versions of one package must refuse");
        assert_eq!(
            error,
            CatalogIdentityError::DuplicateMember {
                field: "release-packages",
                id: "base".into(),
            }
        );
    }

    #[test]
    fn attachment_and_registration_targets_are_exact() {
        let mut wrong_version = attachment();
        wrong_version.wiring_version = 2;
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::from([("orders".to_string(), wrong_version)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "base".into(),
                wiring_id: "orders".into(),
                wiring_version: 2,
            })
        );

        let mut missing = registration();
        missing.wiring_id = "ghost".into();
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::new(),
                BTreeMap::from([("overlay::orders-changed".to_string(), missing)]),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "overlay".into(),
                wiring_id: "ghost".into(),
                wiring_version: 2,
            })
        );

        let mut wrong_package = attachment();
        wrong_package.package_id = "overlay".into();
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::from([("orders".to_string(), wrong_package)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "overlay".into(),
                wiring_id: "orders".into(),
                wiring_version: 3,
            })
        );
    }

    #[test]
    fn registration_identity_keeps_owner_and_emitter_distinct() {
        let mut registrations = BTreeMap::from([
            (
                "base::receipt-created".to_owned(),
                ServingRegistration {
                    package_id: "base".into(),
                    source_package_id: "base".into(),
                    wiring_id: "orders".into(),
                    wiring_version: 3,
                    entity: "receipt".into(),
                    ops: BTreeSet::from(["insert".into()]),
                    input: ServingRegistrationInput::Event,
                },
            ),
            ("overlay::receipt-created".to_owned(), registration()),
        ]);
        ServingManifest::new(
            release(),
            components(),
            wirings(),
            BTreeMap::new(),
            registrations.clone(),
        )
        .expect("the same local registration id remains distinct by owner package");

        let overlay = registrations
            .get_mut("overlay::receipt-created")
            .expect("fixture carries the overlay registration");
        overlay.source_package_id = "missing".into();
        let error = ServingManifest::new(
            release(),
            components(),
            wirings(),
            BTreeMap::new(),
            registrations,
        )
        .expect_err("the emitter must be an exact member of the release");
        assert!(
            error
                .to_string()
                .contains("absent from the effective release")
        );
    }

    #[test]
    fn registration_map_key_is_owner_qualified() {
        for key in ["orders-changed", "base::orders-changed", "overlay::bad::id"] {
            let error = ServingManifest::new(
                release(),
                components(),
                wirings(),
                BTreeMap::new(),
                BTreeMap::from([(key.to_owned(), registration())]),
            )
            .expect_err("a registration key must carry its exact owner coordinate");
            assert!(error.to_string().contains("registration key"));
        }
    }

    #[test]
    fn removed_flow_and_plan_fields_are_refused() {
        let mut document = serde_json::to_value(manifest()).expect("manifest serializes");
        document["flows"] = serde_json::json!({});
        assert!(serde_json::from_value::<ServingManifest>(document).is_err());
    }

    #[test]
    fn the_delivery_ceiling_is_enforced_at_the_reader() {
        let mount = vec![b'x'; MAX_SERVING_MANIFEST_BYTES + 1];
        assert_eq!(
            ServingManifest::from_canonical_bytes(&mount),
            Err(CatalogIdentityError::ManifestTooLarge {
                bytes: MAX_SERVING_MANIFEST_BYTES + 1,
                limit: MAX_SERVING_MANIFEST_BYTES,
            })
        );
    }

    #[test]
    fn the_configmap_name_is_a_dns_1123_subdomain() {
        let name = release_manifest_configmap_name(COMPONENT_A).expect("digest names a map");
        assert_eq!(
            name,
            "release-manifest-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }
}
