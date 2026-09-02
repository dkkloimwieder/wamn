//! Portable connection-type semantics owned beside component admission.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

        assert_eq!(owner_of(ConnectionField::RelativeTarget), ConnectionFieldOwner::Author);
        assert_eq!(owner_of(ConnectionField::Body), ConnectionFieldOwner::Author);
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
