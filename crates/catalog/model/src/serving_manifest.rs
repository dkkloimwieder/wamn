//! The immutable release-serving manifest mounted by every serving process.
//!
//! Format 2 closes over component digests, wiring definitions, attachments, and
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
    ArtifactHash, AttachmentKind, CatalogIdentityError, DefinitionHash, HASH_PREFIX,
    ManifestDigest, validate_digest, validate_text,
};

/// The only serving-manifest format admitted by this revision.
pub const SERVING_MANIFEST_FORMAT_VERSION: u32 = 2;

/// The attachment auth-policy mode that permits an unauthenticated caller.
pub const NO_AUTHENTICATION_MODE: &str = "none";

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
    pub catalog_id: String,
    pub catalog_version: u32,
    pub environment: String,
}

/// One immutable component artifact in the release closure.
///
/// The full tuple is identity: one component/interface pair may legitimately
/// need more than one digest when different admitted operations are packaged as
/// distinct single-operation artifacts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponent {
    pub component: String,
    pub interface_version: String,
    pub digest: ArtifactHash,
}

/// One immutable wiring definition in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingWiring {
    pub wiring_id: String,
    pub wiring_version: u32,
    pub graph_hash: DefinitionHash,
}

/// One release attachment targeting an exact wiring identity and version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingAttachment {
    pub kind: AttachmentKind,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub definition_hash: DefinitionHash,
    pub definition: Value,
    pub auth_policy: Value,
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
        ManifestDigest::parse(wamn_execution_contract::canonical_json_sha256(&self.as_value()))
            .expect("the shared canonicalizer emits a canonical sha256 digest")
    }

    /// Parse, validate, and admit only canonical format-2 bytes.
    ///
    /// The version is classified before the format-2 schema is decoded. This is
    /// what makes a format-1 mount an explicit typed refusal rather than a
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
        validate_text(&self.release.catalog_id, "catalog-id")?;
        validate_text(&self.release.environment, "environment")?;
        if self.release.catalog_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "catalog-version",
            });
        }

        for component in &self.components {
            validate_text(&component.component, "component")?;
            validate_text(&component.interface_version, "interface-version")?;
        }

        let mut targets = BTreeSet::new();
        for wiring in &self.wirings {
            validate_text(&wiring.wiring_id, "wiring-id")?;
            if wiring.wiring_version == 0 {
                return Err(CatalogIdentityError::ZeroVersion {
                    field: "wiring-version",
                });
            }
            if !targets.insert((wiring.wiring_id.as_str(), wiring.wiring_version)) {
                return invalid("a wiring identity-version pair occurs more than once");
            }
        }

        for (attachment_id, attachment) in &self.attachments {
            validate_text(attachment_id, "attachment-id")?;
            validate_wiring_target(&targets, &attachment.wiring_id, attachment.wiring_version)?;
            if !attachment.definition.is_object() || !attachment.auth_policy.is_object() {
                return invalid("attachment definition and resolved source must be JSON objects");
            }
            if contains_retired_identity(&attachment.definition)
                || contains_retired_identity(&attachment.auth_policy)
            {
                return invalid("attachment configuration carries retired flow or plan identity");
            }
        }

        for (registration_id, registration) in &self.registrations {
            validate_text(registration_id, "registration-id")?;
            validate_wiring_target(
                &targets,
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
    targets: &BTreeSet<(&str, u32)>,
    wiring_id: &str,
    wiring_version: u32,
) -> Result<(), CatalogIdentityError> {
    validate_text(wiring_id, "wiring-id")?;
    if wiring_version == 0 {
        return Err(CatalogIdentityError::ZeroVersion {
            field: "wiring-version",
        });
    }
    if !targets.contains(&(wiring_id, wiring_version)) {
        return Err(CatalogIdentityError::UnresolvableManifestWiring {
            wiring_id: wiring_id.to_string(),
            wiring_version,
        });
    }
    Ok(())
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
            catalog_id: "cat".into(),
            catalog_version: 7,
            environment: "prod".into(),
        }
    }

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([
            ServingComponent {
                component: "transform".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_B),
            },
            ServingComponent {
                component: "http-request".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_A),
            },
        ])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([
            ServingWiring {
                wiring_id: "shipping".into(),
                wiring_version: 2,
                graph_hash: definition_hash(GRAPH_B),
            },
            ServingWiring {
                wiring_id: "orders".into(),
                wiring_version: 3,
                graph_hash: definition_hash(GRAPH_A),
            },
        ])
    }

    fn attachment() -> ServingAttachment {
        ServingAttachment {
            kind: AttachmentKind::Http,
            wiring_id: "orders".into(),
            wiring_version: 3,
            definition_hash: definition_hash(DEFINITION),
            definition: serde_json::json!({
                "id": "orders",
                "kind": "http",
                "route": {"host": "*", "path": "/orders", "method": "POST"}
            }),
            auth_policy: serde_json::json!({"mode": "none"}),
        }
    }

    fn registration() -> ServingRegistration {
        ServingRegistration {
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
            BTreeMap::from([("orders-changed".to_string(), registration())]),
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
            BTreeMap::from([("orders-changed".to_string(), registration())]),
        )
        .expect("reordered fixture is valid");

        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
    }

    #[test]
    fn only_canonical_format_two_bytes_are_admitted() {
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
    fn format_one_is_a_typed_refusal_not_a_compatibility_arm() {
        let legacy = br#"{"attachments":{},"flows":{},"format-version":"0.1","registrations":{},"release":{"catalog-id":"cat","catalog-version":1,"environment":"prod","tenant-id":"t1"}}"#;
        let error = ServingManifest::from_canonical_bytes(legacy)
            .expect_err("format one must never enter the format-two decoder");
        assert_eq!(
            error,
            CatalogIdentityError::UnsupportedServingManifestVersion {
                requested: "0.1".into()
            }
        );
        assert!(
            error
                .to_string()
                .starts_with(UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL)
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
                BTreeMap::from([("orders-changed".to_string(), missing)]),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                wiring_id: "ghost".into(),
                wiring_version: 2,
            })
        );
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
