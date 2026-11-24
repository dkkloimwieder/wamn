//! Credential mounts for an operation Job.

use crate::job;
use serde_json::{Value, json};
use std::path::Path;
use wamn_registry::name::REGISTRY_PULL;

/// Mount the installed OCI client configuration for the selected credential source.
pub fn mount_registry(job: &mut Value, metadata: bool) {
    let spec = &mut job["spec"]["template"]["spec"];
    let volume = if metadata {
        json!({"name":"registry","configMap":{"name":"wamn-registry-helper"}})
    } else {
        spec["imagePullSecrets"] = json!([{"name":REGISTRY_PULL}]);
        json!({"name":"registry","secret":{"secretName":REGISTRY_PULL,"defaultMode":256,
            "items":[{"key":".dockerconfigjson","path":"config.json"}]}})
    };
    spec["volumes"].as_array_mut().unwrap().push(volume);
    job::container(job)["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"registry","mountPath":"/credentials/registry","readOnly":true}));
    job::environment(job, &[("DOCKER_CONFIG", "/credentials/registry")]);
}

/// Mount the CA used to verify the broker's TLS certificate.
pub fn mount_broker_ca(job: &mut Value) {
    job["spec"]["template"]["spec"]["volumes"].as_array_mut().expect("volumes").push(
        json!({"name":"nats-ca","configMap":{"name":"nats-ca","items":[{"key":"ca.crt","path":"ca.crt"}]}}),
    );
    crate::job::container(job)["volumeMounts"]
        .as_array_mut()
        .expect("mounts")
        .push(json!({"name":"nats-ca","mountPath":"/etc/wamn/nats-ca","readOnly":true}));
}

pub fn mount_identity_worker_token(job: &mut Value, token_file: &Path, audience: &str) {
    job["spec"]["template"]["spec"]["volumes"].as_array_mut().expect("volumes").extend([
        json!({"name":"identity-ca","configMap":{"name":"identity-ca"}}),
        json!({"name":"identity-token","projected":{"sources":[{"serviceAccountToken":{
            "audience":audience,"path":token_file.file_name().and_then(std::ffi::OsStr::to_str).expect("token file name"),"expirationSeconds":3600
        }}]}}),
    ]);
    crate::job::container(job)["volumeMounts"].as_array_mut().expect("mounts").extend([
        json!({"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true}),
        json!({"name":"identity-token","mountPath":token_file.parent().expect("token directory"),"readOnly":true}),
    ]);
}

/// Mount the worker's credential staging directory.
pub fn mount_database_files(job: &mut Value) {
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .expect("worker volumes")
        .push(json!({"name":"database-files","emptyDir":{"medium":"Memory"}}));
    job::container(job)["volumeMounts"]
        .as_array_mut()
        .expect("worker volume mounts")
        .push(json!({"name":"database-files","mountPath":wamn_platform_install::database::DATABASE_DIR}));
}
