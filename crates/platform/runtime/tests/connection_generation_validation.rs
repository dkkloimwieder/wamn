use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde_json::{Value, json};
use wamn_runtime::connection_authority::{AuthorityError, DnsResolver, NetworkPolicy};
use wamn_runtime::connection_generation::{
    ClusterNetworkPolicySnapshot, ConnectionContractSnapshot, CredentialKind,
    CredentialKindSnapshot, GenerationValidationErrorKind, GenerationValidationSnapshot,
    HTTP_CONNECTION_CONTRACT, HTTP_CONNECTION_TYPE, PlatformHostPolicySnapshot,
    StagedConnectionGeneration, ValidationInputIdentity, definition_hash,
    validate_staged_connection_generation,
};
use wash_runtime::host::allowed_hosts::AllowedHost;

#[derive(Debug)]
struct FixedDns(HashMap<String, Vec<SocketAddr>>);

impl DnsResolver for FixedDns {
    fn resolve(
        &self,
        host: &str,
        _port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
        std::future::ready(Ok(self.0.get(host).cloned().unwrap_or_default()))
    }
}

#[derive(Debug)]
struct ExactNetwork(Vec<SocketAddr>);

impl NetworkPolicy for ExactNetwork {
    fn allows(&self, address: SocketAddr) -> bool {
        self.0.contains(&address)
    }
}

fn socket(octet: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet)), port)
}

fn allowed(value: &str) -> AllowedHost {
    value.parse().expect("allowed-host fixture parses")
}

fn identity(id: &str, revision: &str) -> ValidationInputIdentity {
    ValidationInputIdentity {
        id: id.into(),
        revision: revision.into(),
    }
}

fn definition() -> Value {
    json!({
        "primary-authority": "https://erp.example/api/",
        "failover-authorities": ["https://erp-backup.example/api/"],
        "tls-verification": "verify-authority",
        "tls-names": ["erp.example", "erp-backup.example"],
        "redirect-policy": "same-authority",
        "proxy-transport": null,
        "credential-set-handle": "erp-api"
    })
}

struct Fixture {
    contract: ConnectionContractSnapshot,
    credential: CredentialKindSnapshot,
    hosts: Vec<AllowedHost>,
    network: ExactNetwork,
    dns: FixedDns,
}

impl Fixture {
    fn new() -> Self {
        let primary = socket(1, 443);
        let failover = socket(2, 443);
        Self {
            contract: ConnectionContractSnapshot {
                identity: identity("http-contract", "descriptor-v1"),
                requirement_type: HTTP_CONNECTION_TYPE.into(),
                contract: HTTP_CONNECTION_CONTRACT.into(),
                allowed_credential_kinds: [CredentialKind::HttpHeader].into(),
            },
            credential: CredentialKindSnapshot {
                identity: identity("erp-api", "kind-rev-7"),
                handle: "erp-api".into(),
                kind: CredentialKind::HttpHeader,
            },
            hosts: vec![
                allowed("https://erp.example"),
                allowed("https://erp-backup.example"),
            ],
            network: ExactNetwork(vec![primary, failover]),
            dns: FixedDns(HashMap::from([
                ("erp.example".into(), vec![primary]),
                ("erp-backup.example".into(), vec![failover]),
            ])),
        }
    }

    fn snapshot<'a>(
        &'a self,
        credential: Option<&'a CredentialKindSnapshot>,
        hosts: &'a [AllowedHost],
        network: &'a ExactNetwork,
    ) -> GenerationValidationSnapshot<'a, ExactNetwork> {
        GenerationValidationSnapshot {
            connection_contract: &self.contract,
            credential_kind: credential,
            platform_host_policy: PlatformHostPolicySnapshot {
                identity: identity("platform-host-policy", "host-rev-11"),
                allowed_hosts: hosts,
            },
            cluster_network_policy: ClusterNetworkPolicySnapshot {
                identity: identity("cluster-network-policy", "network-rev-13"),
                policy: network,
            },
        }
    }
}

fn candidate<'a>(definition: &'a Value) -> StagedConnectionGeneration<'a> {
    StagedConnectionGeneration {
        requirement_type: HTTP_CONNECTION_TYPE,
        contract: HTTP_CONNECTION_CONTRACT,
        definition_hash: Box::leak(definition_hash(definition).into_boxed_str()),
        definition,
    }
}

async fn error_for(fixture: &Fixture, definition: &Value) -> GenerationValidationErrorKind {
    validate_staged_connection_generation(
        &candidate(definition),
        &fixture.snapshot(Some(&fixture.credential), &fixture.hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect_err("mutated generation must be refused")
    .kind()
}

#[tokio::test]
async fn compatible_generation_records_every_validated_input_identity() {
    let fixture = Fixture::new();
    let definition = definition();
    let validated = validate_staged_connection_generation(
        &candidate(&definition),
        &fixture.snapshot(Some(&fixture.credential), &fixture.hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect("compatible staged generation");

    assert_eq!(validated.requirement_type.as_ref(), HTTP_CONNECTION_TYPE);
    assert_eq!(validated.contract.as_ref(), HTTP_CONNECTION_CONTRACT);
    assert_eq!(
        validated.definition_hash.as_ref(),
        definition_hash(&definition)
    );
    assert_eq!(validated.primary_authority.host(), "erp.example");
    assert_eq!(
        validated.failover_authorities[0].host(),
        "erp-backup.example"
    );
    assert!(validated.proxy_authority.is_none());
    assert_eq!(
        validated.connection_contract_input.revision.as_ref(),
        "descriptor-v1"
    );
    assert_eq!(
        validated.credential_kind_input.revision.as_ref(),
        "kind-rev-7"
    );
    assert_eq!(
        validated.platform_host_policy_input.revision.as_ref(),
        "host-rev-11"
    );
    assert_eq!(
        validated.cluster_network_policy_input.revision.as_ref(),
        "network-rev-13"
    );
}

#[tokio::test]
async fn exact_type_contract_hash_and_field_ownership_fail_precisely() {
    let fixture = Fixture::new();
    let definition = definition();
    let hash = definition_hash(&definition);
    for (requirement_type, contract, expected) in [
        (
            "postgres",
            HTTP_CONNECTION_CONTRACT,
            GenerationValidationErrorKind::UnsupportedType,
        ),
        (
            HTTP_CONNECTION_TYPE,
            "wamn:connection/http@0.2.0",
            GenerationValidationErrorKind::UnsupportedContract,
        ),
    ] {
        let error = validate_staged_connection_generation(
            &StagedConnectionGeneration {
                requirement_type,
                contract,
                definition_hash: &hash,
                definition: &definition,
            },
            &fixture.snapshot(Some(&fixture.credential), &fixture.hosts, &fixture.network),
            &fixture.dns,
        )
        .await
        .expect_err("wrong type or contract must fail");
        assert_eq!(error.kind(), expected);
    }

    let error = validate_staged_connection_generation(
        &StagedConnectionGeneration {
            requirement_type: HTTP_CONNECTION_TYPE,
            contract: HTTP_CONNECTION_CONTRACT,
            definition_hash: "wrong-hash",
            definition: &definition,
        },
        &fixture.snapshot(Some(&fixture.credential), &fixture.hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect_err("definition hash mismatch must fail");
    assert_eq!(
        error.kind(),
        GenerationValidationErrorKind::DefinitionHashMismatch
    );

    let mut missing = definition.clone();
    missing.as_object_mut().expect("object").remove("tls-names");
    assert_eq!(
        error_for(&fixture, &missing).await,
        GenerationValidationErrorKind::MissingField
    );
    let mut unknown = definition.clone();
    unknown["future-knob"] = json!(true);
    assert_eq!(
        error_for(&fixture, &unknown).await,
        GenerationValidationErrorKind::UnknownField
    );
    let mut forbidden = definition.clone();
    forbidden["method"] = json!("POST");
    assert_eq!(
        error_for(&fixture, &forbidden).await,
        GenerationValidationErrorKind::ForbiddenField
    );
}

#[tokio::test]
async fn canonical_authority_tls_redirect_and_proxy_coherence_are_closed() {
    let fixture = Fixture::new();

    let mut noncanonical = definition();
    noncanonical["primary-authority"] = json!("HTTPS://ERP.Example:443/api/");
    assert_eq!(
        error_for(&fixture, &noncanonical).await,
        GenerationValidationErrorKind::NonCanonicalAuthority
    );
    let mut duplicate = definition();
    duplicate["failover-authorities"] = json!(["https://erp.example/api/"]);
    duplicate["tls-names"] = json!(["erp.example", "erp.example"]);
    assert_eq!(
        error_for(&fixture, &duplicate).await,
        GenerationValidationErrorKind::DuplicateAuthority
    );
    let mut tls = definition();
    tls["tls-names"] = json!(["other.example", "erp-backup.example"]);
    assert_eq!(
        error_for(&fixture, &tls).await,
        GenerationValidationErrorKind::TlsIdentityMismatch
    );
    let mut redirect = definition();
    redirect["redirect-policy"] = json!("cross-authority");
    assert_eq!(
        error_for(&fixture, &redirect).await,
        GenerationValidationErrorKind::RedirectPolicyMismatch
    );
    let mut proxy = definition();
    proxy["proxy-authority"] = json!("http://proxy.internal:8080/");
    assert_eq!(
        error_for(&fixture, &proxy).await,
        GenerationValidationErrorKind::ProxyMismatch
    );

    proxy["proxy-transport"] = json!("connect");
    assert_eq!(
        error_for(&fixture, &proxy).await,
        GenerationValidationErrorKind::UnsupportedTransport
    );
}

#[tokio::test]
async fn credential_validation_uses_kind_metadata_only() {
    let fixture = Fixture::new();
    let definition = definition();

    let missing = validate_staged_connection_generation(
        &candidate(&definition),
        &fixture.snapshot(None, &fixture.hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect_err("missing credential metadata must fail");
    assert_eq!(
        missing.kind(),
        GenerationValidationErrorKind::CredentialMissing
    );

    let wrong_kind = CredentialKindSnapshot {
        identity: identity("erp-api", "kind-rev-8"),
        handle: "erp-api".into(),
        kind: CredentialKind::OAuth2Bearer,
    };
    let mismatch = validate_staged_connection_generation(
        &candidate(&definition),
        &fixture.snapshot(Some(&wrong_kind), &fixture.hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect_err("contract-forbidden credential kind must fail");
    assert_eq!(
        mismatch.kind(),
        GenerationValidationErrorKind::CredentialKindMismatch
    );
}

#[tokio::test]
async fn both_snapshotted_outer_policy_ceiling_denials_are_typed() {
    let fixture = Fixture::new();
    let definition = definition();
    let denied_hosts = [allowed("https://other.example")];
    let host_error = validate_staged_connection_generation(
        &candidate(&definition),
        &fixture.snapshot(Some(&fixture.credential), &denied_hosts, &fixture.network),
        &fixture.dns,
    )
    .await
    .expect_err("platform host ceiling must deny");
    assert_eq!(
        host_error.kind(),
        GenerationValidationErrorKind::PlatformHostPolicyDenied
    );

    let denied_network = ExactNetwork(Vec::new());
    let network_error = validate_staged_connection_generation(
        &candidate(&definition),
        &fixture.snapshot(Some(&fixture.credential), &fixture.hosts, &denied_network),
        &fixture.dns,
    )
    .await
    .expect_err("cluster network ceiling must deny");
    assert_eq!(
        network_error.kind(),
        GenerationValidationErrorKind::ClusterNetworkPolicyDenied
    );
}

// wamn-hopk R5: a test grepping this crate's own source for the words "current
// DNS", "redirect", "proxy" and "outer-policy" is deleted. Dispatch-time
// enforcement is proven by the behavioural arms above, which call the validator.
