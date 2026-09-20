//! Human membership on the deployed route.

use std::fs;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::{
    ReceivingCluster, apply, assert_source_unchanged, checked, evidence_directory, kubectl,
    released_http, resources, start, write_private,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn human_membership_and_permission_revocation() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = evidence_directory()?;
    Box::pin(super::with_signals(&evidence, run_membership(&evidence))).await
}

async fn run_membership(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true).await?;
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
    result?;
    if let Some((candidate, manifest)) = &cluster.resources.candidate {
        wamn_control::delivery::report_candidate_success(candidate, manifest)?;
    }
    Ok(())
}
