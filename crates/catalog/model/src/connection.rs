//! Portable connection-type semantics owned beside component admission.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The runtime that executes an integration type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeType {
    Host,
    Wasm,
    Native,
}

impl std::fmt::Display for RuntimeType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Host => "host",
            Self::Wasm => "wasm",
            Self::Native => "native",
        })
    }
}

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

/// The connection types that connection binding can bind today. The enum is
/// the closed vocabulary a caller chooses from, so a descriptor is never
/// authored from a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementType {
    Blobstore,
    Http,
}

impl RequirementType {
    /// Resolve the compiled connector implementation for a catalog integration.
    pub fn for_integration(code: &str) -> Option<Self> {
        match code {
            "http" => Some(Self::Http),
            "gcs" | "s3" => Some(Self::Blobstore),
            _ => None,
        }
    }

    pub fn descriptor(self) -> ConnectionTypeDescriptor {
        match self {
            Self::Blobstore => ConnectionTypeDescriptor::blobstore_v1(),
            Self::Http => ConnectionTypeDescriptor::http_v1(),
        }
    }

    /// The coordinates the type's plugin reads from a generation definition.
    /// For blobstore these are the ones `wamn_blobstore::binding::resolve`
    /// demands for the definition's `provider`, and nothing else: a key nobody
    /// reads is a key nobody validates. An `s3` definition may omit `provider`.
    fn coordinates(self, provider: Option<&str>) -> Result<&'static [&'static str], String> {
        match (self, provider) {
            (Self::Http, _) => Ok(&["primary-authority", "tls-verification"]),
            (Self::Blobstore, None) => Ok(&["endpoint", "container", "prefix"]),
            (Self::Blobstore, Some("s3")) => Ok(&["provider", "endpoint", "container", "prefix"]),
            (Self::Blobstore, Some("gcs")) => Ok(&["provider", "container", "prefix"]),
            (Self::Blobstore, Some(other)) => Err(format!(
                "the generation definition's provider {other:?} is neither s3 nor gcs"
            )),
        }
    }

    /// Check a generation definition against the coordinates this type's
    /// plugin reads, and return the reason it is refused. `wamn-ctl
    /// binding refuses with it.
    pub fn check_definition(self, definition: &serde_json::Value) -> Result<(), String> {
        use serde_json::Value;
        let Some(object) = definition.as_object() else {
            return Err("the generation definition must be a JSON object".to_owned());
        };
        let provider = match object.get("provider") {
            None => None,
            Some(Value::String(provider)) => Some(provider.as_str()),
            Some(_) => {
                return Err("the generation definition's provider must be a string".to_owned());
            }
        };
        let coordinates = self.coordinates(provider)?;
        for coordinate in coordinates {
            match object.get(*coordinate) {
                Some(Value::String(value)) if !value.is_empty() => {}
                Some(Value::String(_)) => {
                    return Err(format!(
                        "the generation definition's {coordinate} is empty; {self:?} needs it"
                    ));
                }
                Some(_) => {
                    return Err(format!(
                        "the generation definition's {coordinate} must be a string; \
                         {self:?} reads it as one"
                    ));
                }
                None => {
                    return Err(format!(
                        "the generation definition lacks {coordinate}; \
                         {self:?} reads it at resolve time"
                    ));
                }
            }
        }
        if self == Self::Blobstore
            && let Some(value) = object.get("cdn_host")
        {
            let value = value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "cdn_host must be a nonempty HTTP origin".to_owned())?;
            let origin = if value.contains("://") {
                value.to_owned()
            } else {
                format!("https://{value}")
            };
            let url = url::Url::parse(&origin)
                .map_err(|_| "cdn_host must be an HTTP origin".to_owned())?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(
                    "cdn_host must be an HTTP origin without a path, query, or credentials"
                        .to_owned(),
                );
            }
        }
        if let Some(key) = object.keys().find(|key| {
            !(coordinates.contains(&key.as_str())
                || self == Self::Blobstore && key.as_str() == "cdn_host")
        }) {
            return Err(format!(
                "the generation definition carries {key}, which {self:?} never reads; \
                 a coordinate nobody reads is a coordinate nobody validates"
            ));
        }
        Ok(())
    }

    /// Check the credential handle of a checked definition. A `gcs`
    /// definition takes none, because the host signs with its pod's service
    /// account, and every other definition needs one.
    pub fn check_credential_handle(
        self,
        definition: &serde_json::Value,
        credential_handle: Option<&str>,
    ) -> Result<(), String> {
        if credential_handle.is_some_and(|handle| handle.starts_with("org:")) {
            return Err("the org: credential prefix is reserved for org connections".to_owned());
        }
        let gcs = definition
            .get("provider")
            .and_then(serde_json::Value::as_str)
            == Some("gcs");
        match (self, gcs, credential_handle) {
            (Self::Blobstore, true, Some(_)) => Err(
                "a gcs definition takes no credential handle; the host signs with its pod's service account"
                    .to_owned(),
            ),
            (Self::Blobstore, false, None | Some("")) | (Self::Http, _, None | Some("")) => Err(
                "the credential handle must not be empty; the host resolves it by name".to_owned(),
            ),
            _ => Ok(()),
        }
    }
}

/// Derive the credential handle of an org connection.
pub fn org_credential_handle(org: &str, name: &str) -> Result<String, String> {
    let handle = format!("org:{org}:{name}");
    parse_org_credential_handle(&handle)?;
    Ok(handle)
}

/// Parse an org handle without accepting ambiguous or empty coordinates.
pub fn parse_org_credential_handle(handle: &str) -> Result<(&str, &str), String> {
    let invalid = || "invalid org credential handle; expected org:<org>:<name>".to_owned();
    let (org, name) = handle
        .strip_prefix("org:")
        .and_then(|tail| tail.split_once(':'))
        .ok_or_else(invalid)?;
    let name = if let Some((name, generation)) = name.split_once('@') {
        if generation.parse::<i64>().ok().is_none_or(|n| n < 1) {
            return Err(invalid());
        }
        name
    } else {
        name
    };
    if org.is_empty()
        || org.len() > 40
        || !org.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        || org == "wamn"
        || org.starts_with("wamn-")
        || name.is_empty()
        || name.len() > 63
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(invalid());
    }
    Ok((org, name))
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

    #[test]
    fn integration_dispatch_refuses_an_unimplemented_catalog_code() {
        assert_eq!(
            RequirementType::for_integration("http"),
            Some(RequirementType::Http)
        );
        assert_eq!(
            RequirementType::for_integration("gcs"),
            Some(RequirementType::Blobstore)
        );
        assert_eq!(
            RequirementType::for_integration("s3"),
            Some(RequirementType::Blobstore)
        );
        assert_eq!(RequirementType::for_integration("unimplemented"), None);
    }

    #[test]
    fn http_uses_the_existing_descriptor() {
        assert_eq!(
            RequirementType::Http.descriptor(),
            ConnectionTypeDescriptor::http_v1()
        );
    }

    #[test]
    fn an_http_definition_needs_exactly_the_coordinates_the_plugin_reads() {
        RequirementType::Http
            .check_definition(&serde_json::json!({
                "primary-authority": "https://erp.example", "tls-verification": "verify-authority"
            }))
            .expect("the two coordinates the plugin reads are a complete definition");
    }

    #[test]
    fn a_missing_http_primary_authority_is_refused_by_name() {
        let error = RequirementType::Http
            .check_definition(&serde_json::json!({"tls-verification": "verify-authority"}))
            .expect_err("primary-authority is read at resolve time");
        assert_eq!(
            error,
            "the generation definition lacks primary-authority; Http reads it at resolve time"
        );
    }

    #[test]
    fn a_missing_http_tls_verification_is_refused_by_name() {
        let error = RequirementType::Http
            .check_definition(&serde_json::json!({"primary-authority": "https://erp.example"}))
            .expect_err("tls-verification is read at resolve time");
        assert_eq!(
            error,
            "the generation definition lacks tls-verification; Http reads it at resolve time"
        );
    }

    #[test]
    fn an_extra_http_coordinate_is_refused_by_name() {
        for key in ["provider", "endpoint", "headers"] {
            let mut definition = serde_json::json!({
                "primary-authority": "https://erp.example", "tls-verification": "verify-authority"
            });
            definition[key] = serde_json::json!("unused");
            assert_eq!(
                RequirementType::Http
                    .check_definition(&definition)
                    .unwrap_err(),
                format!(
                    "the generation definition carries {key}, which Http never reads; a coordinate nobody reads is a coordinate nobody validates"
                )
            );
        }
    }

    #[test]
    fn an_empty_http_coordinate_is_refused_by_name() {
        for coordinate in ["primary-authority", "tls-verification"] {
            let mut definition = serde_json::json!({
                "primary-authority": "https://erp.example", "tls-verification": "verify-authority"
            });
            definition[coordinate] = serde_json::json!("");
            let error = RequirementType::Http
                .check_definition(&definition)
                .expect_err("the plugin needs a nonempty coordinate");
            assert_eq!(
                error,
                format!("the generation definition's {coordinate} is empty; Http needs it")
            );
        }
    }

    #[test]
    fn a_nonstring_http_coordinate_is_refused_by_name() {
        for coordinate in ["primary-authority", "tls-verification"] {
            let mut definition = serde_json::json!({
                "primary-authority": "https://erp.example", "tls-verification": "verify-authority"
            });
            definition[coordinate] = serde_json::json!(42);
            assert_eq!(
                RequirementType::Http
                    .check_definition(&definition)
                    .unwrap_err(),
                format!(
                    "the generation definition's {coordinate} must be a string; Http reads it as one"
                )
            );
        }
    }

    #[test]
    fn refuses_a_connection_definition_its_type_refuses() {
        for requirement in [RequirementType::Http, RequirementType::Blobstore] {
            assert_eq!(
                requirement
                    .check_definition(&serde_json::json!("not an object"))
                    .unwrap_err(),
                "the generation definition must be a JSON object"
            );
        }
    }

    #[test]
    fn reads_an_http_connection_and_refuses_an_unknown_type() {
        assert_eq!(
            serde_json::from_value::<RequirementType>(serde_json::json!("http")).unwrap(),
            RequirementType::Http
        );
        assert_eq!(
            serde_json::from_value::<RequirementType>(serde_json::json!("ftp"))
                .unwrap_err()
                .to_string(),
            "unknown variant `ftp`, expected `blobstore` or `http`"
        );
    }

    #[test]
    fn http_needs_a_nonempty_credential_handle() {
        let definition = serde_json::json!({
            "primary-authority": "http://http-fixture:8080/", "tls-verification": "disabled"
        });
        for handle in [None, Some("")] {
            assert_eq!(
                RequirementType::Http.check_credential_handle(&definition, handle),
                Err(
                    "the credential handle must not be empty; the host resolves it by name"
                        .to_owned()
                )
            );
        }
        RequirementType::Http
            .check_credential_handle(&definition, Some("erp"))
            .expect("the host resolves a named credential");
    }

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
    #[test]
    fn blobstore_cdn_host_is_optional_and_accepts_only_an_http_origin() {
        for provider in ["gcs", "s3"] {
            let mut definition =
                serde_json::json!({"provider": provider, "container": "files", "prefix": "app"});
            if provider == "s3" {
                definition["endpoint"] = "https://s3.example.com".into();
            }
            RequirementType::Blobstore
                .check_definition(&definition)
                .unwrap();
            for origin in [
                "cdn.example.com",
                "https://cdn.example.com",
                "http://localhost:9000/",
            ] {
                definition["cdn_host"] = origin.into();
                RequirementType::Blobstore
                    .check_definition(&definition)
                    .unwrap();
            }
            for origin in [
                serde_json::json!(null),
                serde_json::json!(42),
                serde_json::json!(""),
                serde_json::json!("https://cdn.example.com/path"),
                serde_json::json!("https://user:pass@cdn.example.com"),
                serde_json::json!("https://cdn.example.com?query"),
                serde_json::json!("ftp://cdn.example.com"),
            ] {
                definition["cdn_host"] = origin;
                assert!(
                    RequirementType::Blobstore
                        .check_definition(&definition)
                        .is_err()
                );
            }
        }
    }
}

#[cfg(test)]
mod org_handle_tests {
    use super::*;

    #[test]
    fn org_handles_round_trip_and_reserve_the_prefix() {
        let handle = org_credential_handle("acme", "erp_main").unwrap();
        assert_eq!(handle, "org:acme:erp_main");
        assert_eq!(
            parse_org_credential_handle(&handle).unwrap(),
            ("acme", "erp_main")
        );
        assert!(
            RequirementType::Http
                .check_credential_handle(&serde_json::json!({}), Some(&handle))
                .is_err()
        );
        for bad in [
            "erp",
            "org:",
            "org::erp",
            "org:acme:",
            "org:acme:erp:other",
            "org:ACME:erp",
            "org:acme:1erp",
            "org:wamn:erp",
        ] {
            assert!(parse_org_credential_handle(bad).is_err(), "{bad}");
        }
    }
}
