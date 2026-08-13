//! Intrinsic validation for immutable staged HTTP connection generations.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use wamn_flow::node_contract::normalize_portable_http_target;
use wash_runtime::host::allowed_hosts::AllowedHost;

use crate::connection_authority::{
    AuthorityErrorKind, CanonicalAuthority, DnsResolver, HttpConnectionAuthority, NetworkPolicy,
    TlsPolicy, parse_http_connection_authority, resolve_http_request,
};

/// The sole connection type supported by the initial generation validator.
pub const HTTP_CONNECTION_TYPE: &str = "http";

/// The exact typed HTTP contract supported by the initial generation validator.
pub const HTTP_CONNECTION_CONTRACT: &str = "wamn:connection/http@0.1.0";

/// Credential kinds recognized by connection-contract snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CredentialKind {
    HttpHeader,
    OAuth2Bearer,
}

/// Stable identity of one snapshotted validation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationInputIdentity {
    pub id: Box<str>,
    pub revision: Box<str>,
}

/// Exact supported contract record captured for validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionContractSnapshot {
    pub identity: ValidationInputIdentity,
    pub requirement_type: Box<str>,
    pub contract: Box<str>,
    pub allowed_credential_kinds: Box<[CredentialKind]>,
}

/// Non-secret credential metadata captured for validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialKindSnapshot {
    pub identity: ValidationInputIdentity,
    pub handle: Box<str>,
    pub kind: CredentialKind,
}

/// Platform-host policy captured for validation.
#[derive(Debug, Clone)]
pub struct PlatformHostPolicySnapshot<'a> {
    pub identity: ValidationInputIdentity,
    pub allowed_hosts: &'a [AllowedHost],
}

/// Cluster-network policy captured for validation.
#[derive(Debug)]
pub struct ClusterNetworkPolicySnapshot<'a, N> {
    pub identity: ValidationInputIdentity,
    pub policy: &'a N,
}

/// Immutable candidate supplied to intrinsic validation.
#[derive(Debug, Clone)]
pub struct StagedConnectionGeneration<'a> {
    pub requirement_type: &'a str,
    pub contract: &'a str,
    pub definition_hash: &'a str,
    pub definition: &'a Value,
}

/// Complete immutable snapshot used by one intrinsic validation decision.
#[derive(Debug)]
pub struct GenerationValidationSnapshot<'a, N> {
    pub connection_contract: &'a ConnectionContractSnapshot,
    pub credential_kind: Option<&'a CredentialKindSnapshot>,
    pub platform_host_policy: PlatformHostPolicySnapshot<'a>,
    pub cluster_network_policy: ClusterNetworkPolicySnapshot<'a, N>,
}

/// Input identities and canonical authorities proven by intrinsic validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedConnectionGeneration {
    pub definition_hash: Box<str>,
    pub requirement_type: Box<str>,
    pub contract: Box<str>,
    pub primary_authority: CanonicalAuthority,
    pub failover_authorities: Box<[CanonicalAuthority]>,
    pub proxy_authority: Option<CanonicalAuthority>,
    pub connection_contract_input: ValidationInputIdentity,
    pub credential_kind_input: ValidationInputIdentity,
    pub platform_host_policy_input: ValidationInputIdentity,
    pub cluster_network_policy_input: ValidationInputIdentity,
}

/// Stable classification for one refused staged generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationValidationErrorKind {
    UnsupportedType,
    UnsupportedContract,
    StaleContractSnapshot,
    DefinitionHashMismatch,
    DefinitionNotObject,
    MissingField,
    UnknownField,
    ForbiddenField,
    InvalidField,
    NonCanonicalAuthority,
    DuplicateAuthority,
    TlsIdentityMismatch,
    RedirectPolicyMismatch,
    ProxyMismatch,
    UnsupportedTransport,
    CredentialMissing,
    CredentialKindMismatch,
    InvalidInputIdentity,
    PlatformHostPolicyDenied,
    ClusterNetworkPolicyDenied,
    PolicyResolutionFailed,
}

/// Typed, fail-closed generation validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationValidationError {
    kind: GenerationValidationErrorKind,
    field: Option<Box<str>>,
    detail: Box<str>,
}

impl GenerationValidationError {
    fn new(kind: GenerationValidationErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            field: None,
            detail: detail.into(),
        }
    }

    fn field(
        kind: GenerationValidationErrorKind,
        field: &str,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            field: Some(field.into()),
            detail: detail.into(),
        }
    }

    pub fn kind(&self) -> GenerationValidationErrorKind {
        self.kind
    }

    pub fn field_name(&self) -> Option<&str> {
        self.field.as_deref()
    }
}

impl std::fmt::Display for GenerationValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GenerationValidationError {}

struct ParsedDefinition {
    primary: HttpConnectionAuthority,
    failovers: Vec<HttpConnectionAuthority>,
    credential_handle: Box<str>,
}

/// Validate a staged HTTP generation against immutable non-secret snapshots.
///
/// This does not activate the generation and is not a dispatch-time policy
/// certificate. Dispatch must still enforce current DNS, redirect, proxy, and
/// outer-policy state.
pub async fn validate_staged_connection_generation<R, N>(
    candidate: &StagedConnectionGeneration<'_>,
    snapshot: &GenerationValidationSnapshot<'_, N>,
    dns: &R,
) -> Result<ValidatedConnectionGeneration, GenerationValidationError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    validate_contract(candidate, snapshot.connection_contract)?;
    validate_identity(
        &snapshot.connection_contract.identity,
        "connection-contract",
    )?;
    validate_identity(
        &snapshot.platform_host_policy.identity,
        "platform-host-policy",
    )?;
    validate_identity(
        &snapshot.cluster_network_policy.identity,
        "cluster-network-policy",
    )?;

    let actual_hash = definition_hash(candidate.definition);
    if candidate.definition_hash != actual_hash {
        return Err(GenerationValidationError::new(
            GenerationValidationErrorKind::DefinitionHashMismatch,
            "staged definition hash does not identify the supplied immutable definition",
        ));
    }

    let parsed = parse_definition(candidate.definition)?;
    let credential = snapshot.credential_kind.ok_or_else(|| {
        GenerationValidationError::field(
            GenerationValidationErrorKind::CredentialMissing,
            "credential-set-handle",
            "referenced credential set does not exist",
        )
    })?;
    validate_identity(&credential.identity, "credential-kind")?;
    if credential.handle.as_ref() != parsed.credential_handle.as_ref() {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::CredentialMissing,
            "credential-set-handle",
            "credential-kind snapshot does not identify the referenced handle",
        ));
    }
    if !snapshot
        .connection_contract
        .allowed_credential_kinds
        .contains(&credential.kind)
    {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::CredentialKindMismatch,
            "credential-set-handle",
            "credential kind is not permitted by the exact connection contract",
        ));
    }

    validate_policy(&parsed.primary, snapshot, dns).await?;
    for failover in &parsed.failovers {
        validate_policy(failover, snapshot, dns).await?;
    }

    Ok(ValidatedConnectionGeneration {
        definition_hash: actual_hash.into(),
        requirement_type: candidate.requirement_type.into(),
        contract: candidate.contract.into(),
        primary_authority: parsed.primary.authority().clone(),
        failover_authorities: parsed
            .failovers
            .iter()
            .map(|authority| authority.authority().clone())
            .collect(),
        proxy_authority: parsed.primary.proxy_authority().cloned(),
        connection_contract_input: snapshot.connection_contract.identity.clone(),
        credential_kind_input: credential.identity.clone(),
        platform_host_policy_input: snapshot.platform_host_policy.identity.clone(),
        cluster_network_policy_input: snapshot.cluster_network_policy.identity.clone(),
    })
}

/// Compute the canonical SHA-256 identity of a staged JSON definition.
pub fn definition_hash(definition: &Value) -> String {
    let bytes = serde_json::to_vec(definition).expect("JSON values always serialize");
    hex::encode(Sha256::digest(bytes))
}

fn validate_contract(
    candidate: &StagedConnectionGeneration<'_>,
    contract: &ConnectionContractSnapshot,
) -> Result<(), GenerationValidationError> {
    if candidate.requirement_type != HTTP_CONNECTION_TYPE {
        return Err(GenerationValidationError::new(
            GenerationValidationErrorKind::UnsupportedType,
            "staged generation has an unsupported connection type",
        ));
    }
    if candidate.contract != HTTP_CONNECTION_CONTRACT {
        return Err(GenerationValidationError::new(
            GenerationValidationErrorKind::UnsupportedContract,
            "staged generation has an unsupported connection contract",
        ));
    }
    if contract.requirement_type.as_ref() != HTTP_CONNECTION_TYPE
        || contract.contract.as_ref() != HTTP_CONNECTION_CONTRACT
    {
        return Err(GenerationValidationError::new(
            GenerationValidationErrorKind::StaleContractSnapshot,
            "connection-contract snapshot is not the exact supported HTTP contract",
        ));
    }
    Ok(())
}

fn validate_identity(
    identity: &ValidationInputIdentity,
    label: &str,
) -> Result<(), GenerationValidationError> {
    if identity.id.is_empty() || identity.revision.is_empty() {
        Err(GenerationValidationError::new(
            GenerationValidationErrorKind::InvalidInputIdentity,
            format!("{label} snapshot identity and revision must be non-empty"),
        ))
    } else {
        Ok(())
    }
}

fn parse_definition(definition: &Value) -> Result<ParsedDefinition, GenerationValidationError> {
    let object = definition.as_object().ok_or_else(|| {
        GenerationValidationError::new(
            GenerationValidationErrorKind::DefinitionNotObject,
            "staged connection definition must be a JSON object",
        )
    })?;
    validate_fields(object)?;

    let primary_value = string_field(object, "primary-authority")?;
    let failover_values = string_array_field(object, "failover-authorities")?;
    let tls = parse_tls(string_field(object, "tls-verification")?)?;
    let tls_names = string_array_field(object, "tls-names")?;
    validate_redirect(string_field(object, "redirect-policy")?)?;
    let proxy_value = optional_string_field(object, "proxy-authority")?;
    validate_proxy_transport(object, proxy_value)?;
    let credential_handle = string_field(object, "credential-set-handle")?;
    if credential_handle.is_empty() {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::InvalidField,
            "credential-set-handle",
            "credential-set-handle must be non-empty",
        ));
    }

    let primary = parse_canonical(primary_value, tls, proxy_value, "primary-authority")?;
    let mut failovers = Vec::with_capacity(failover_values.len());
    for value in failover_values {
        failovers.push(parse_canonical(
            value,
            tls,
            proxy_value,
            "failover-authorities",
        )?);
    }
    validate_unique_authorities(&primary, &failovers)?;
    validate_tls_names(tls, &primary, &failovers, &tls_names)?;

    Ok(ParsedDefinition {
        primary,
        failovers,
        credential_handle: credential_handle.into(),
    })
}

fn validate_fields(object: &Map<String, Value>) -> Result<(), GenerationValidationError> {
    const REQUIRED: [&str; 7] = [
        "primary-authority",
        "failover-authorities",
        "tls-verification",
        "tls-names",
        "redirect-policy",
        "credential-set-handle",
        "proxy-transport",
    ];
    const OPTIONAL: [&str; 1] = ["proxy-authority"];
    const FORBIDDEN: [&str; 6] = [
        "method",
        "relative-target",
        "headers",
        "body",
        "idempotency-key",
        "credential-secret",
    ];

    for field in REQUIRED {
        if !object.contains_key(field) {
            return Err(GenerationValidationError::field(
                GenerationValidationErrorKind::MissingField,
                field,
                format!("staged connection definition is missing {field:?}"),
            ));
        }
    }
    for field in object.keys() {
        if FORBIDDEN.contains(&field.as_str()) {
            return Err(GenerationValidationError::field(
                GenerationValidationErrorKind::ForbiddenField,
                field,
                format!("field {field:?} is not environment-owned"),
            ));
        }
        if !REQUIRED.contains(&field.as_str()) && !OPTIONAL.contains(&field.as_str()) {
            return Err(GenerationValidationError::field(
                GenerationValidationErrorKind::UnknownField,
                field,
                format!("unknown staged connection field {field:?}"),
            ));
        }
    }
    Ok(())
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, GenerationValidationError> {
    object[field].as_str().ok_or_else(|| {
        GenerationValidationError::field(
            GenerationValidationErrorKind::InvalidField,
            field,
            format!("field {field:?} must be a string"),
        )
    })
}

fn optional_string_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, GenerationValidationError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(GenerationValidationError::field(
            GenerationValidationErrorKind::InvalidField,
            field,
            format!("field {field:?} must be a non-empty string or null"),
        )),
    }
}

fn string_array_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Vec<&'a str>, GenerationValidationError> {
    let values = object[field].as_array().ok_or_else(|| {
        GenerationValidationError::field(
            GenerationValidationErrorKind::InvalidField,
            field,
            format!("field {field:?} must be an array of strings"),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    GenerationValidationError::field(
                        GenerationValidationErrorKind::InvalidField,
                        field,
                        format!("field {field:?} must contain only non-empty strings"),
                    )
                })
        })
        .collect()
}

fn parse_tls(value: &str) -> Result<TlsPolicy, GenerationValidationError> {
    match value {
        "disabled" => Ok(TlsPolicy::Disabled),
        "verify-authority" => Ok(TlsPolicy::VerifyAuthority),
        _ => Err(GenerationValidationError::field(
            GenerationValidationErrorKind::TlsIdentityMismatch,
            "tls-verification",
            "TLS verification must be disabled or verify-authority",
        )),
    }
}

fn validate_redirect(value: &str) -> Result<(), GenerationValidationError> {
    if matches!(value, "deny" | "same-authority") {
        Ok(())
    } else {
        Err(GenerationValidationError::field(
            GenerationValidationErrorKind::RedirectPolicyMismatch,
            "redirect-policy",
            "HTTP redirect policy must be deny or same-authority",
        ))
    }
}

fn validate_proxy_transport(
    object: &Map<String, Value>,
    proxy: Option<&str>,
) -> Result<(), GenerationValidationError> {
    let transport = object["proxy-transport"].as_str();
    let coherent = match proxy {
        Some(_) => transport == Some("connect"),
        None => object["proxy-transport"].is_null(),
    };
    if !coherent {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::ProxyMismatch,
            "proxy-transport",
            "proxy transport must be connect exactly when a proxy authority is configured",
        ));
    }
    if proxy.is_some() {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::UnsupportedTransport,
            "proxy-transport",
            "HTTP connection v1 supports direct transport only",
        ));
    }
    Ok(())
}

fn parse_canonical(
    value: &str,
    tls: TlsPolicy,
    proxy: Option<&str>,
    field: &str,
) -> Result<HttpConnectionAuthority, GenerationValidationError> {
    let authority = parse_http_connection_authority(value, tls, proxy).map_err(|error| {
        GenerationValidationError::field(
            GenerationValidationErrorKind::NonCanonicalAuthority,
            field,
            error.to_string(),
        )
    })?;
    if authority.canonical_base_url() != value
        || proxy.is_some_and(|value| authority.canonical_proxy_url() != Some(value))
    {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::NonCanonicalAuthority,
            field,
            format!("field {field:?} is not in canonical URL form"),
        ));
    }
    Ok(authority)
}

fn validate_unique_authorities(
    primary: &HttpConnectionAuthority,
    failovers: &[HttpConnectionAuthority],
) -> Result<(), GenerationValidationError> {
    let mut unique = BTreeSet::new();
    unique.insert(primary.authority());
    for failover in failovers {
        if !unique.insert(failover.authority()) {
            return Err(GenerationValidationError::field(
                GenerationValidationErrorKind::DuplicateAuthority,
                "failover-authorities",
                "primary and failover authorities must be unique",
            ));
        }
    }
    Ok(())
}

fn validate_tls_names(
    tls: TlsPolicy,
    primary: &HttpConnectionAuthority,
    failovers: &[HttpConnectionAuthority],
    names: &[&str],
) -> Result<(), GenerationValidationError> {
    let authorities = std::iter::once(primary).chain(failovers);
    let expected_count = if tls == TlsPolicy::VerifyAuthority {
        failovers.len() + 1
    } else {
        0
    };
    if names.len() != expected_count
        || (tls == TlsPolicy::VerifyAuthority
            && !authorities
                .zip(names)
                .all(|(authority, name)| authority.authority().host() == *name && name.is_ascii()))
    {
        return Err(GenerationValidationError::field(
            GenerationValidationErrorKind::TlsIdentityMismatch,
            "tls-names",
            "TLS names must exactly match each HTTPS primary/failover authority in order",
        ));
    }
    Ok(())
}

async fn validate_policy<R, N>(
    authority: &HttpConnectionAuthority,
    snapshot: &GenerationValidationSnapshot<'_, N>,
    dns: &R,
) -> Result<(), GenerationValidationError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    let target = normalize_portable_http_target("/__authority_validation__")
        .expect("fixed validation target uses the portable spelling");
    resolve_http_request(
        authority,
        &target,
        snapshot.platform_host_policy.allowed_hosts,
        snapshot.cluster_network_policy.policy,
        dns,
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        let kind = match error.kind() {
            AuthorityErrorKind::PlatformHostDenied => {
                GenerationValidationErrorKind::PlatformHostPolicyDenied
            }
            AuthorityErrorKind::NetworkDenied => {
                GenerationValidationErrorKind::ClusterNetworkPolicyDenied
            }
            AuthorityErrorKind::DnsResolutionFailed => {
                GenerationValidationErrorKind::PolicyResolutionFailed
            }
            _ => GenerationValidationErrorKind::NonCanonicalAuthority,
        };
        GenerationValidationError::new(kind, error.to_string())
    })
}
