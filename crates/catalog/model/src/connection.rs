//! Portable connection-type semantics owned beside component admission.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A portable component-owned connection requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentConnectionRequirement {
    component_digest: String,
    store_alias: String,
    requirement: ConnectionTypeDescriptor,
}

impl ComponentConnectionRequirement {
    /// Construct one requirement from its admitted component identity.
    pub fn new(
        component_digest: impl Into<String>,
        store_alias: impl Into<String>,
        requirement: ConnectionTypeDescriptor,
    ) -> Self {
        Self {
            component_digest: component_digest.into(),
            store_alias: store_alias.into(),
            requirement,
        }
    }

    pub fn component_digest(&self) -> &str {
        &self.component_digest
    }

    pub fn store_alias(&self) -> &str {
        &self.store_alias
    }

    pub fn requirement(&self) -> &ConnectionTypeDescriptor {
        &self.requirement
    }

    /// Canonical environment-independent bytes persisted with the component.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("portable component connection requirement serializes")
    }

    /// SHA-256 of [`Self::canonical_bytes`].
    pub fn requirement_hash(&self) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(self.canonical_bytes());
        let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
        output.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
        }
        output
    }
}

/// Shape version for portable connection-type descriptors.
pub const CONNECTION_DESCRIPTOR_VERSION: &str = "1";

/// A field whose ownership is fixed by a connection-type descriptor.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionField {
    Method,
    RelativeTarget,
    Headers,
    Body,
    Authority,
    Tls,
    Redirect,
    Proxy,
    Credential,
    /// Object-store container the binding confines the component to.
    Bucket,
    /// Key prefix within [`ConnectionField::Bucket`] that walls the component
    /// off from the rest of the container.
    Prefix,
}

/// The principal allowed to supply one connection field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionFieldOwner {
    Author,
    Environment,
    System,
}

/// Canonical ownership for one connection field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionFieldOwnership {
    pub field: ConnectionField,
    pub owner: ConnectionFieldOwner,
}

/// The authority interpretation fixed by a connection type.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionAuthorityModel {
    HttpOrigin,
    /// Endpoint plus a fixed container and key prefix. Unlike an HTTP origin,
    /// the authority is not just *where* to reach but *how far in*: the bucket
    /// and prefix are walls, and a key that escapes them is refused rather
    /// than redirected.
    ObjectStoreBucket,
}

/// How environment-owned credentials enter a request.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialInjection {
    EnvironmentSelectedHttpHeader,
    /// The host signs the request itself. Unlike a header injection, no
    /// credential-shaped value ever exists in a structure the guest composes,
    /// names or observes — the signature is computed host-side over a request
    /// the guest can only describe, so there is no boundary for credential
    /// bytes to cross.
    HostSignedRequest,
}

/// Versioned portable semantics for one connection type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionTypeDescriptor {
    pub descriptor_version: String,
    pub requirement_type: String,
    pub contract: String,
    pub authority_model: ConnectionAuthorityModel,
    pub field_ownership: Vec<ConnectionFieldOwnership>,
    pub credential_injection: CredentialInjection,
}

impl ConnectionTypeDescriptor {
    /// The minimum portable HTTP connection descriptor.
    pub fn http_v1() -> Self {
        let author = ConnectionFieldOwner::Author;
        let environment = ConnectionFieldOwner::Environment;
        Self {
            descriptor_version: CONNECTION_DESCRIPTOR_VERSION.to_owned(),
            requirement_type: "http".to_owned(),
            contract: "wamn:connection/http@0.1.0".to_owned(),
            authority_model: ConnectionAuthorityModel::HttpOrigin,
            field_ownership: vec![
                ownership(ConnectionField::Method, author),
                ownership(ConnectionField::RelativeTarget, author),
                ownership(ConnectionField::Headers, author),
                ownership(ConnectionField::Body, author),
                ownership(ConnectionField::Authority, environment),
                ownership(ConnectionField::Tls, environment),
                ownership(ConnectionField::Redirect, environment),
                ownership(ConnectionField::Proxy, environment),
                ownership(ConnectionField::Credential, environment),
            ],
            credential_injection: CredentialInjection::EnvironmentSelectedHttpHeader,
        }
    }

    /// The minimum portable object-store connection descriptor.
    ///
    /// The author owns only what varies per call — the object key, relative to
    /// the prefix wall, and the body. Everything that constitutes authority is
    /// environment-owned: the endpoint, the bucket, the prefix that confines
    /// the component within it, and the credential. That split is the
    /// confinement: an author can name an object, and cannot name a container.
    pub fn blobstore_v1() -> Self {
        let author = ConnectionFieldOwner::Author;
        let environment = ConnectionFieldOwner::Environment;
        Self {
            descriptor_version: CONNECTION_DESCRIPTOR_VERSION.to_owned(),
            requirement_type: "blobstore".to_owned(),
            contract: "wasmcloud:blobstore/blobstore@0.1.0".to_owned(),
            authority_model: ConnectionAuthorityModel::ObjectStoreBucket,
            field_ownership: vec![
                ownership(ConnectionField::RelativeTarget, author),
                ownership(ConnectionField::Body, author),
                ownership(ConnectionField::Authority, environment),
                ownership(ConnectionField::Bucket, environment),
                ownership(ConnectionField::Prefix, environment),
                ownership(ConnectionField::Credential, environment),
            ],
            credential_injection: CredentialInjection::HostSignedRequest,
        }
    }

    /// Stable bytes embedded in persisted connection requirement identities.
    pub fn identity_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("connection descriptor identity serializes")
    }
}

fn ownership(field: ConnectionField, owner: ConnectionFieldOwner) -> ConnectionFieldOwnership {
    ConnectionFieldOwnership { field, owner }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confinement is the split itself: an author may name an OBJECT, and may
    /// not name a CONTAINER. If a bucket or prefix ever became author-owned,
    /// the guest could address the whole store and the wall would be gone.
    #[test]
    fn the_blobstore_author_may_name_an_object_but_never_a_container() {
        let descriptor = ConnectionTypeDescriptor::blobstore_v1();
        let owner_of = |field: ConnectionField| {
            descriptor
                .field_ownership
                .iter()
                .find(|entry| entry.field == field)
                .unwrap_or_else(|| panic!("{field:?} must be owned by someone"))
                .owner
        };

        assert_eq!(
            owner_of(ConnectionField::RelativeTarget),
            ConnectionFieldOwner::Author
        );
        assert_eq!(
            owner_of(ConnectionField::Body),
            ConnectionFieldOwner::Author
        );
        for walled in [
            ConnectionField::Authority,
            ConnectionField::Bucket,
            ConnectionField::Prefix,
            ConnectionField::Credential,
        ] {
            assert_eq!(
                owner_of(walled),
                ConnectionFieldOwner::Environment,
                "{walled:?} is authority; author ownership would breach the confinement"
            );
        }
    }

    /// The credential never enters a structure the guest composes. Header
    /// injection puts a credential-shaped value into a request the guest
    /// authored; host-signing does not, which is why blobstore uses it.
    #[test]
    fn the_blobstore_credential_is_host_signed_not_header_injected() {
        assert_eq!(
            ConnectionTypeDescriptor::blobstore_v1().credential_injection,
            CredentialInjection::HostSignedRequest
        );
        assert_eq!(
            ConnectionTypeDescriptor::http_v1().credential_injection,
            CredentialInjection::EnvironmentSelectedHttpHeader
        );
    }

    /// A descriptor's serialized bytes ARE its persisted requirement identity,
    /// so the two descriptors must never collide, and neither may quietly
    /// become the other.
    #[test]
    fn each_descriptor_has_its_own_identity() {
        let http = ConnectionTypeDescriptor::http_v1();
        let blobstore = ConnectionTypeDescriptor::blobstore_v1();

        assert_ne!(http.identity_bytes(), blobstore.identity_bytes());
        assert_eq!(http.requirement_type, "http");
        assert_eq!(blobstore.requirement_type, "blobstore");
        assert_eq!(blobstore.contract, "wasmcloud:blobstore/blobstore@0.1.0");
        assert_eq!(blobstore.descriptor_version, CONNECTION_DESCRIPTOR_VERSION);
    }

    /// `requirement_hash` is persisted in `catalog.connection_requirements` and
    /// compared on promotion, so its bytes must never move. The golden value is
    /// the sha256 of the literal JSON below, computed outside this code.
    #[test]
    fn the_requirement_hash_bytes_are_pinned() {
        let requirement = ComponentConnectionRequirement::new(
            "sha256:component-a",
            "erp",
            ConnectionTypeDescriptor::http_v1(),
        );

        assert_eq!(
            String::from_utf8(requirement.canonical_bytes()).expect("utf-8 JSON"),
            concat!(
                r#"{"component-digest":"sha256:component-a","store-alias":"erp","#,
                r#""requirement":{"descriptor-version":"1","requirement-type":"http","#,
                r#""contract":"wamn:connection/http@0.1.0","authority-model":"http-origin","#,
                r#""field-ownership":[{"field":"method","owner":"author"},"#,
                r#"{"field":"relative-target","owner":"author"},"#,
                r#"{"field":"headers","owner":"author"},{"field":"body","owner":"author"},"#,
                r#"{"field":"authority","owner":"environment"},"#,
                r#"{"field":"tls","owner":"environment"},"#,
                r#"{"field":"redirect","owner":"environment"},"#,
                r#"{"field":"proxy","owner":"environment"},"#,
                r#"{"field":"credential","owner":"environment"}],"#,
                r#""credential-injection":"environment-selected-http-header"}}"#,
            )
        );
        assert_eq!(
            requirement.requirement_hash(),
            "sha256:39bc5f123de98ee830fe3aade1121f9ae8b3164c196f5f2d9664810c680bbb7c"
        );
    }

    /// Adding a variant to a single-variant enum inside a `deny_unknown_fields`
    /// descriptor is a wire change: an older host deserializing the new value
    /// hard-fails rather than degrading. Pin the wire spellings so that change
    /// is deliberate and visible in a diff.
    #[test]
    fn the_new_wire_spellings_are_pinned() {
        let json = serde_json::to_string(&ConnectionTypeDescriptor::blobstore_v1())
            .expect("descriptor serializes");
        for spelling in [
            "\"object-store-bucket\"",
            "\"host-signed-request\"",
            "\"bucket\"",
            "\"prefix\"",
        ] {
            assert!(json.contains(spelling), "missing {spelling} in {json}");
        }
    }
}
