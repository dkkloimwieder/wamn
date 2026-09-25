//! The release bundle, the echo guest and the session keys that the edge tests
//! share.

#![allow(
    dead_code,
    reason = "each test binary uses part of the shared fixtures"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::json;
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ArtifactHash, AttachmentKind, AttachmentTarget,
    ComponentPackageScope, DefinitionHash, EffectiveReleaseId, OperationKind, PackageCoordinate,
    RELEASE_MANIFEST_FILE_NAME, ServingAttachment, ServingComponent, ServingComponentOperation,
    ServingManifest, ServingRelease, ServingRoute,
};
use wamn_edge::config::{EdgeConfig, HttpConfig, ReleaseConfig, SessionConfig, StoreConfig};
use wamn_edge::grants::GRANTS_FILE_NAME;
use wamn_edge::release::{BUNDLE_FILE_NAME, COMPONENTS_FILE_NAME, INGRESS_FILE_NAME, file_digest};
use wamn_edge::serve::{EdgeHost, serve};
use wamn_session::keys::PublicSessionKey;
use wamn_session::token::{SessionAuthority, SessionClaims, SessionHeader};

pub const TENANT: &str = "t1";
pub const PACKAGE: &str = "scale";
pub const VERSION: &str = "1.0.0";
pub const COMPONENT: &str = "device";
pub const PERMISSION: &str = "scale:device/record@1.0.0";
/// A registered export is keyed by its permission identity.
pub const OPERATION: &str = PERMISSION;
pub const ATTACHMENT: &str = "record-http";
pub const HOST: &str = "edge.localhost";
pub const PATH: &str = "/record";
pub const ISSUER: &str = "https://identity.example.test";
pub const ORG: &str = "org-a";
pub const AUDIENCE: &str = "tenant-a/edge";
pub const PRINCIPAL: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";

/// A node component whose one export returns its input as its emission.
pub fn echo_guest() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
      (import "wamn:node/types@0.1.0" (instance $node
        (type $json' string)
        (export "json" (type $json (eq $json')))
        (type $context' (record
          (field "wiring-id" string) (field "wiring-version" u32)
          (field "node-id" string) (field "delivery-id" string)
          (field "input-port" (option string)) (field "occurrence" u32)
          (field "traceparent" (option string)) (field "tracestate" (option string))
          (field "deadline-ms" (option u64)) (field "config" $json)))
        (export "node-context" (type $context (eq $context')))
        (type $detail' (record (field "message" string) (field "code" (option string))))
        (export "error-detail" (type $detail (eq $detail')))
        (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (export "rate-limit-detail" (type $rate (eq $rate')))
        (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
          (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (export "node-error" (type $error (eq $error')))
        (type $emission' (record (field "payload" $json) (field "port" (option string))))
        (export "emission" (type $emission (eq $emission')))))
      (alias export $node "json" (type $json))
      (alias export $node "node-context" (type $context))
      (alias export $node "node-error" (type $error))
      (alias export $node "emission" (type $emission))
      (core module $memory
        (memory (export "memory") 16)
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param $old i32) (param $old-size i32)
          (param $align i32) (param $size i32) (result i32) (local $new i32)
          global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $new
          local.get $size i32.add global.set $next
          global.get $next i32.const 1048576 i32.gt_u if unreachable end
          local.get $old if
            local.get $new local.get $old local.get $old-size memory.copy
          end local.get $new))
      (core instance $memory (instantiate $memory))
      (core func $return (canon task.return (result (result $emission (error $error)))
        (memory $memory "memory")))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "return" (func $return
          (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
        (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
        (func (export "run") (param $input i32) (result i32)
          i32.const 0 local.get $input i32.load offset=96 local.get $input i32.load offset=100
          i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i64.const 0
          call $return i32.const 0))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "return" (func $return))))))
      (func $run async (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{OPERATION}" (instance $handler)))"#
    ))
    .expect("encode the echo guest")
}

pub fn manifest(guest: &[u8]) -> ServingManifest {
    let definition = json!({
        "id": ATTACHMENT,
        "kind": "http",
        "route": {"host": HOST, "path": PATH, "method": "POST"},
    });
    let definition_hash = file_digest(&serde_json::to_vec(&definition).expect("definition"));
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
            digest: ArtifactHash::parse(file_digest(guest)).expect("digest"),
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
        BTreeSet::from([ServingRoute {
            package_id: PACKAGE.into(),
            component: COMPONENT.into(),
            operation: OPERATION.into(),
            kind: OperationKind::Command,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
        }]),
        BTreeSet::new(),
        BTreeMap::from([(
            ATTACHMENT.to_owned(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                package_id: PACKAGE.into(),
                target: AttachmentTarget::Route {
                    component: COMPONENT.into(),
                    operation: OPERATION.into(),
                },
                definition_hash: DefinitionHash::parse(definition_hash).expect("hash"),
                definition,
                auth_policy: json!({"modes": ["session"]}),
                registered_operation: Some(PERMISSION.into()),
            },
        )]),
        BTreeMap::new(),
    )
    .expect("the fixture manifest is valid")
}

pub fn fact(guest: &[u8]) -> AdmittedComponent {
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
        component_digest: file_digest(guest),
        imports: vec!["wamn:node/types@0.1.0".into()],
        imports_fingerprint: file_digest(b"wamn:node/types@0.1.0"),
        effects: Vec::new(),
    }
}

/// A key pair from a fixed seed, and its public JWK under `kid`.
pub fn key(kid: &str, seed: u8) -> (Ed25519KeyPair, PublicSessionKey) {
    let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("seed");
    let public = PublicSessionKey {
        kid: kid.into(),
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        alg: "Ed25519".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    };
    (pair, public)
}

/// A session for `roles`, signed by `pair` under `kid`.
pub fn session(pair: &Ed25519KeyPair, kid: &str, roles: &[&str]) -> String {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    )
    .expect("the clock fits in i64");
    let claims = SessionClaims {
        iss: ISSUER.into(),
        sub: PRINCIPAL.into(),
        org: ORG.into(),
        aud: AUDIENCE.into(),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        exp: now + 300,
        iat: now - 10,
        jti: "token-1".into(),
        authority: SessionAuthority::Login(PRINCIPAL.into()),
        csrf: None,
    };
    let header =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&SessionHeader::new(kid)).expect("header"));
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims"));
    let signed = format!("{header}.{body}");
    let signature = URL_SAFE_NO_PAD.encode(pair.sign(signed.as_bytes()).as_ref());
    format!("{signed}.{signature}")
}

pub fn write(directory: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(directory.join(name), bytes).expect("write a bundle file");
}

/// Write the bundle, the key file and nothing else into a directory named for
/// the test, and return the directory with the bundle digest.
pub fn bundle(test: &str, ingress: &[u8], public: &PublicSessionKey) -> (PathBuf, String) {
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create the bundle directory");
    let guest = echo_guest();
    let manifest = manifest(&guest).canonical_bytes();
    let components = serde_json::to_vec(&[fact(&guest)]).expect("facts");
    let grants =
        serde_json::to_vec(&json!({"roles": {"operator": [PERMISSION], "device": [PERMISSION]}}))
            .expect("grants");
    let digest = file_digest(&guest);
    let tag = digest.strip_prefix("sha256:").expect("digest prefix");
    write(&directory, RELEASE_MANIFEST_FILE_NAME, &manifest);
    write(&directory, COMPONENTS_FILE_NAME, &components);
    write(&directory, GRANTS_FILE_NAME, &grants);
    write(&directory, INGRESS_FILE_NAME, ingress);
    write(&directory, &format!("{tag}.wasm"), &guest);
    write(
        &directory,
        "session-keys.json",
        &serde_json::to_vec(&json!({"keys": [public]})).expect("keys"),
    );
    let bundle = serde_json::to_vec(&json!({
        "format": 1,
        "manifest": file_digest(&manifest),
        "components": file_digest(&components),
        "grants": file_digest(&grants),
        "ingress": file_digest(ingress),
    }))
    .expect("bundle");
    write(&directory, BUNDLE_FILE_NAME, &bundle);
    (directory, file_digest(&bundle))
}

pub fn ingress() -> Vec<u8> {
    std::fs::read(
        std::env::var("WAMN_FLOW_HTTP_COMPONENT")
            .expect("WAMN_FLOW_HTTP_COMPONENT names the guest"),
    )
    .expect("read the http-route guest")
}

/// The configuration of the edge over the bundle in `directory`, with no
/// device.
pub fn config(directory: &Path, digest: String) -> EdgeConfig {
    EdgeConfig {
        release: ReleaseConfig {
            dir: directory.to_owned(),
            digest,
        },
        session: SessionConfig {
            keys: directory.join("session-keys.json"),
            issuer: ISSUER.into(),
            org: ORG.into(),
            audience: AUDIENCE.into(),
        },
        store: StoreConfig {
            db: directory.join("edge.db"),
        },
        http: HttpConfig {
            route_host: HOST.into(),
            listen: "127.0.0.1:0".parse().expect("address"),
        },
        device: None,
        forward: None,
    }
}

pub async fn start(config: EdgeConfig) -> EdgeHost {
    Box::pin(serve(config))
        .await
        .expect("the edge serves its bundle")
}
