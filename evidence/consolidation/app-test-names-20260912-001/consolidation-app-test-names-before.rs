#![allow(dead_code)]
use std::path::PathBuf;
use serde::Deserialize;
use serde_json::{json,Value};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inputs {
    pub(crate) source: String,
    pub(crate) host_binary: PathBuf,
    pub(crate) host_secrets: PathBuf,
    pub(crate) registry_auth: PathBuf,
    pub(crate) workload: PathBuf,
    pub(crate) pat_secret: PathBuf,
    pub(crate) private_dir: PathBuf,
    pub(crate) evidence_dir: PathBuf,
    pub(crate) nats_url: String,
    pub(crate) scheduler_nats_url: String,
    pub(crate) scheduler_nats_tls_ca: PathBuf,
    pub(crate) scheduler_nats_tls_cert: PathBuf,
    pub(crate) scheduler_nats_tls_key: PathBuf,
    pub(crate) scheduler_client_tls_cert: PathBuf,
    pub(crate) scheduler_client_tls_key: PathBuf,
    pub(crate) otlp_endpoint: String,
    pub(crate) proof_id: String,
    pub(crate) component_artifact_base: String,
    pub(crate) release_artifact_base: String,
    pub(crate) manifest_digest: String,
    pub(crate) org: String,
    pub(crate) project: String,
    pub(crate) schema: String,
    pub(crate) environment: String,
    pub(crate) route_host: String,
    pub(crate) route_path: String,
    pub(crate) probe_body: Value,
    pub(crate) max_concurrent_starts: usize,
}

fn inspect(value:Value)->Value {match serde_json::from_value::<Inputs>(value) {Ok(input)=>json!({"accepted":input.proof_id}),Err(error)=>json!({"error":error.to_string()})}}
fn main(){let fixture:Value=serde_json::from_str(r###"{"source": "boundary-value", "host_binary": "boundary-value", "host_secrets": "boundary-value", "registry_auth": "boundary-value", "workload": "boundary-value", "pat_secret": "boundary-value", "private_dir": "boundary-value", "evidence_dir": "boundary-value", "nats_url": "boundary-value", "scheduler_nats_url": "boundary-value", "scheduler_nats_tls_ca": "boundary-value", "scheduler_nats_tls_cert": "boundary-value", "scheduler_nats_tls_key": "boundary-value", "scheduler_client_tls_cert": "boundary-value", "scheduler_client_tls_key": "boundary-value", "otlp_endpoint": "boundary-value", "proof_id": "boundary-value", "component_artifact_base": "boundary-value", "release_artifact_base": "boundary-value", "manifest_digest": "boundary-value", "org": "boundary-value", "project": "boundary-value", "schema": "boundary-value", "environment": "boundary-value", "route_host": "boundary-value", "route_path": "boundary-value", "probe_body": {"x": "y"}, "max_concurrent_starts": 3}"###).unwrap();
let mut missing=fixture.clone();missing.as_object_mut().unwrap().remove("proof_id");
let mut renamed=missing.clone();renamed["test_id"]=json!("boundary-value");
let mut wrong=fixture.clone();wrong["proof_id"]=json!(3);
let mut unknown=fixture.clone();unknown["extra"]=json!(true);
let rows=vec![inspect(fixture),inspect(missing),inspect(renamed),inspect(wrong),inspect(unknown),inspect(json!([])),inspect(json!(true))];
let mut diagnostics=Vec::new();for stdout in ["", "run completed: wrong\n", "run completed: one\nrun completed: two\n"] {let receipts=stdout.lines().filter(|line|line.starts_with("run completed:")).collect::<Vec<_>>();diagnostics.push(format!("wamn dev returned the wrong product receipt: {receipts:?}; stdout={stdout:?}"));}
println!("{}",json!({"inputs":rows,"diagnostics":diagnostics}));}
