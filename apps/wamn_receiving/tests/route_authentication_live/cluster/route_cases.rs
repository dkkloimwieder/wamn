//! Receiving command histories and human membership on the deployed route.

use std::fs;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{
    ReceivingCluster, apply, assert_source_unchanged, checked, evidence_directory, kubectl,
    materializer_case, postcommit_case, released_http, resources, start, write_private,
};

#[tokio::test]
#[ignore = "builds and runs Receiving command histories against an owned disposable cluster"]
async fn command_histories() -> anyhow::Result<()> {
    let evidence = evidence_directory().await?;
    super::with_signals(&evidence, run_histories(&evidence)).await
}

async fn run_histories(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true, false).await?;
    let result = async {
        let (route, _, _, _) = released_http(&cluster, 3).await?;
        let endpoint = materializer_case::endpoint(&cluster, "receiving-correctness-nodeport").await?;
        postcommit_case::assert_unknown_route(&cluster, &endpoint).await?;
        let mut digests = serde_json::Map::new();
        for component in ["receiving", "client_acme_receiving"] {
            let bytes = fs::read(cluster.artifacts.components.join(format!("{component}.wasm")))?;
            digests.insert(component.into(), json!(format!("sha256:{}", hex::encode(Sha256::digest(bytes)))));
        }
        let package: Value = serde_json::from_slice(&fs::read(cluster.resources.repository
            .join("apps/wamn_receiving/generated/package-weld.json"))?)?;
        let path = evidence.join("receiving-correctness.jsonl");
        let inputs = serde_json::from_value(json!({
            "project_pg_url":route.database_url,"route_endpoint":endpoint,
            "route_host":cluster.inputs.route_host,"route_caller_secret":cluster.inputs.route_caller_secret_output,
            "tenant":super::super::TENANT,"caller_role":"route-caller","evidence_file":path,
            "source_commit":cluster.resources.source,"component_digests":digests,
            "corpus_sha256":package["application_sql_corpus_identity"],"seed":7701,"cases":16,"history":null,
        }))?;
        let cancellation = pg_walstream::CancellationToken::new();
        let _cancel_on_exit = cancellation.clone().drop_guard();
        tokio::task::spawn_blocking(move || super::super::command_histories::assert_histories_with_cancellation(inputs, cancellation))
            .await.context("join the bounded Receiving command histories")??;
        let summaries = fs::read_to_string(path)?.lines().map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?.into_iter().filter(|row| row["case"] == "summary").collect::<Vec<_>>();
        ensure!(summaries.len() == 1 && summaries[0]["result"] == "pass"
            && summaries[0]["generated_cases"] == 16 && summaries[0]["boundary_cases"] == 7
            && summaries[0]["explicit_histories"].as_u64().is_some_and(|count| count > 0)
            && summaries[0].get("reproduction").is_none_or(|value| value == false),
            "Receiving command history results are absent or incomplete");
        fs::write(evidence.join("receiving-correctness-summary.json"), serde_json::to_vec_pretty(&summaries[0])?)?;
        assert_source_unchanged(&cluster.resources).await
    }.await;
    finish(&mut cluster, result).await
}

#[tokio::test]
#[ignore = "builds and runs the retained in-cluster Receiving membership cases"]
async fn human_membership_and_permission_revocation() -> anyhow::Result<()> {
    let evidence = evidence_directory().await?;
    super::with_signals(&evidence, run_membership(&evidence)).await
}

async fn run_membership(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true, false).await?;
    let result = async {
        let (route, _, _, _) = released_http(&cluster, 1).await?;
        membership_job(&cluster, &route.database_url).await?;
        assert_source_unchanged(&cluster.resources).await
    }
    .await;
    finish(&mut cluster, result).await
}

async fn membership_job(cluster: &ReceivingCluster, project_url: &str) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let secret = resources.work.join("membership-secret.json");
    write_private(
        &secret,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"membership-test-fixture","namespace":resources.name},
            "stringData":{"system-url":cluster.inputs.system_pg_url,"project-url":project_url},
        }))?,
    )?;
    apply(resources, &secret).await?;
    let image = resources
        .gates_image
        .as_deref()
        .context("membership uses the built gates image")?;
    let job = json!({"apiVersion":"batch/v1","kind":"Job",
    "metadata":{"name":"membership-test","namespace":resources.name},
    "spec":{"activeDeadlineSeconds":660,"backoffLimit":0,"template":{"spec":{
        "restartPolicy":"Never","automountServiceAccountToken":false,"containers":[{
            "name":"membership-test","image":image,"imagePullPolicy":"Never",
            "command":["/usr/local/bin/wamn-gates","membership-test"],
            "args":["--endpoint-url",format!("http://flow-http.{}.svc.cluster.local",resources.name),
                "--host",cluster.inputs.route_host,"--org",super::super::ORG,
                "--project",super::super::PROJECT,"--env",super::super::ENVIRONMENT,"--tenant",super::super::TENANT],
            "env":[{"name":"WAMN_SYSTEM_ADMIN_URL","valueFrom":{"secretKeyRef":{"name":"membership-test-fixture","key":"system-url"}}},
                {"name":"WAMN_PROJECT_ADMIN_URL","valueFrom":{"secretKeyRef":{"name":"membership-test-fixture","key":"project-url"}}}],
        }]
    }}}});
    let path = resources.work.join("membership-job.json");
    fs::write(&path, serde_json::to_vec_pretty(&job)?)?;
    fs::copy(&path, resources.evidence.join("membership-test-job.json"))?;
    apply(resources, &path).await?;
    checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "wait",
        "--for=condition=Complete",
        "job/membership-test",
        "--timeout=720s",
    ]))
    .await?;
    let job: Value = serde_json::from_slice(
        &checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "get",
            "job",
            "membership-test",
            "-o",
            "json",
        ]))
        .await?,
    )?;
    let pods: Value = serde_json::from_slice(
        &checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "get",
            "pods",
            "-l",
            "job-name=membership-test",
            "-o",
            "json",
        ]))
        .await?,
    )?;
    fs::write(
        resources.evidence.join("membership-test-job.json"),
        serde_json::to_vec_pretty(&job)?,
    )?;
    fs::write(
        resources.evidence.join("membership-test-pod.json"),
        serde_json::to_vec_pretty(&pods)?,
    )?;
    ensure!(
        job["status"]["succeeded"] == 1
            && job["status"].get("failed").is_none_or(|value| value == 0)
            && job["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| conditions
                    .iter()
                    .any(|condition| condition["type"] == "Complete"
                        && condition["status"] == "True")),
        "the membership Job did not complete exactly once without a failed attempt"
    );
    let nodes: Vec<Value> = serde_json::from_slice(&fs::read(
        resources.evidence.join("gates-image/host-image-nodes.json"),
    )?)?;
    let digest = nodes
        .first()
        .and_then(|node| node["runtime_digest"].as_str())
        .context("the loaded gates image has its digest")?;
    let pods = pods["items"]
        .as_array()
        .context("the membership Job has observed pods")?;
    ensure!(pods.len() == 1, "membership must run in one pod");
    let pod = &pods[0];
    ensure!(
        pod["status"]["phase"] == "Succeeded"
            && pod["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners
                    .iter()
                    .any(|owner| owner["uid"] == job["metadata"]["uid"]
                        && owner["kind"] == "Job"
                        && owner["controller"] == true))
            && pod["spec"]["containers"]
                .as_array()
                .is_some_and(|containers| containers.len() == 1)
            && pod["spec"]["containers"][0]["name"] == "membership-test"
            && pod["spec"]["containers"][0]["image"] == image
            && pod["spec"]["containers"][0]["command"]
                == json!(["/usr/local/bin/wamn-gates", "membership-test"])
            && pod["status"]["containerStatuses"]
                .as_array()
                .is_some_and(|containers| containers.len() == 1)
            && pod["status"]["containerStatuses"][0]["name"] == "membership-test"
            && pod["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] == 0
            && pod["status"]["containerStatuses"][0]["imageID"]
                .as_str()
                .is_some_and(|value| value.ends_with(digest)),
        "membership must use its owned Job, exact executable and loaded image"
    );
    let log =
        checked(kubectl(resources).args(["-n", &resources.name, "logs", "job/membership-test"]))
            .await?;
    fs::write(resources.evidence.join("membership-test.log"), &log)?;
    let log = String::from_utf8(log)?;
    let cases = log
        .lines()
        .filter(|line| line.starts_with("MEMBERSHIP_TEST "))
        .collect::<Vec<_>>();
    ensure!(
        cases
            == [
                "MEMBERSHIP_TEST case=absent_membership status=401 result=pass",
                "MEMBERSHIP_TEST case=granted status=200 result=pass",
                "MEMBERSHIP_TEST case=repeated_grant status=200 result=pass",
                "MEMBERSHIP_TEST case=role_removed status=403 result=pass",
                "MEMBERSHIP_TEST case=role_restored status=200 result=pass",
                "MEMBERSHIP_TEST case=revoked status=401 result=pass",
                "MEMBERSHIP_TEST case=repeated_revoke status=401 result=pass",
                "MEMBERSHIP_TEST result=pass cases=7 cleanup=pass",
            ],
        "the deployed membership test must complete its seven exact cases and cleanup"
    );
    checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "delete",
        "secret",
        "membership-test-fixture",
    ]))
    .await?;
    Ok(())
}

pub(super) async fn finish(
    cluster: &mut ReceivingCluster,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    if result.is_err() {
        resources::capture_failure(&cluster.resources).await;
    }
    let cleanup = resources::remove(&mut cluster.resources).await;
    let result = match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("Receiving cleanup also failed: {cleanup:#}")))
        }
    };
    fs::write(
        cluster.resources.evidence.join("result.json"),
        serde_json::to_vec_pretty(&json!({
            "source":cluster.resources.source,"cluster":cluster.resources.name,"passed":result.is_ok(),
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
        }))?,
    )?;
    result
}
