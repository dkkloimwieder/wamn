//! Local checks of the deployed request and restart helpers.
use crate::startup_support::*;
use serde_json::{Value, json};
use std::fs;
use wamn_test_infrastructure::workload::HTTP_PROBE_IMAGE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_trace_accepts_repeated_acquisitions_but_not_repeated_work_or_loading() {
        let mut spans = vec![
            json!({"name":"wamn.component.invoke"}),
            json!({"name":"wamn.postgres.statement"}),
        ];
        for class in ["callable-http", "guest-sql", "callable-http", "guest-sql"] {
            spans.push(json!({"name":"wamn.postgres.acquire", "attributes":[{"key":"wamn.authority_class", "value":{"stringValue":class}}]}));
        }
        let trace = json!({"resourceSpans":[{"scopeSpans":[{"spans":spans}]}]});
        assert!(restart_trace_is_complete(&trace));
        for name in [
            "wamn.component.invoke",
            "wamn.postgres.statement",
            "wamn.component.compile",
            "load_component_bytes",
            "link_components",
        ] {
            let mut invalid = trace.clone();
            invalid["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":name}));
            assert!(!restart_trace_is_complete(&invalid), "{name}");
        }
        assert!(!restart_trace_is_complete(&json!({})));
    }

    #[test]
    fn startup_job_reports_immediate_delayed_and_failed_responses() {
        use std::os::unix::fs::PermissionsExt as _;
        let query = "id=%22selected-packaging%22&note=%22it%27s%20quoted%22";
        let job = request_job(
            "selected-environment",
            "selected-http",
            "selected.example",
            "selected-pat",
            "selected-job",
            ("00000000000000000000000000000014", "1111111111111111"),
            query,
        );
        assert_eq!(
            job["metadata"],
            json!({"name":"selected-job","namespace":"selected-environment"})
        );
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 200);
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["template"]["spec"]["restartPolicy"], "Never");
        let containers = job["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        assert_eq!(containers.len(), 1);
        let container = &containers[0];
        assert_eq!(container["image"], HTTP_PROBE_IMAGE);
        assert_eq!(container["imagePullPolicy"], "IfNotPresent");
        assert_eq!(container["command"], json!(["/bin/sh", "-ec"]));
        let env = container["env"].as_array().unwrap();
        assert_eq!(
            env[0],
            json!({"name":"OPERATOR_PAT","valueFrom":{"secretKeyRef":{"name":"selected-pat","key":"token"}}})
        );
        assert_eq!(
            env[1],
            json!({"name":"ROUTE_HOST","value":"selected.example"})
        );
        assert_eq!(
            env[2],
            json!({"name":"TRACEPARENT","value":"00-00000000000000000000000000000014-1111111111111111-01"})
        );
        assert_eq!(
            env[3],
            json!({"name":"ROUTE_URL","value":format!("http://selected-http.selected-environment.svc.cluster.local/packaging/get?{query}")})
        );
        assert_eq!(env.len(), 4);
        let script = container["args"][0].as_str().unwrap();
        assert!(script.contains("--connect-timeout 5 --max-time 60"));
        assert_eq!(script.matches("-ge 150").count(), 1);

        for (name, codes, succeeds, expected_attempts) in [
            ("immediate", "200", true, 1),
            ("delayed", "404 404 200", true, 3),
            ("failed", "404", false, 0),
        ] {
            let directory = wamn_test_infrastructure::scratch::ScratchRoot(
                std::env::temp_dir().join(format!("wms-startup-test-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir(directory.path()).unwrap();
            let program = directory.path().join("curl");
            fs::write(
                &program,
                r#"#!/bin/sh
set -eu
count=0
if [ -f "$TEST_DIRECTORY/count" ]; then count=$(cat "$TEST_DIRECTORY/count"); fi
count=$((count + 1))
printf '%s\n' "$count" >"$TEST_DIRECTORY/count"
output=''
previous=''
for argument in "$@"; do
  if [ "$previous" = --output ]; then output=$argument; fi
  previous=$argument
done
test -n "$output"
index=0
status=''
for value in $TEST_CODES; do
  index=$((index + 1))
  status=$value
  if [ "$index" -eq "$count" ]; then break; fi
done
printf 'test-body-%s' "$status" >"$output"
printf '%s 0.001 0.002' "$status"
"#,
            )
            .unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            let script = script
                .replace(
                    "/tmp/body",
                    &directory.path().join("body").display().to_string(),
                )
                .replace(
                    "/dev/termination-log",
                    &directory.path().join("result.json").display().to_string(),
                );
            // The retained failed-response test shortens only the retry window.
            let script = if succeeds {
                script
            } else {
                script.replace("-ge 150", "-ge 2")
            };
            let mut command = std::process::Command::new("sh");
            command
                .args(["-ec", &script])
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        directory.path().display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .env("TEST_DIRECTORY", directory.path())
                .env("TEST_CODES", codes)
                .env("OPERATOR_PAT", "local-test-token");
            for entry in &env[1..] {
                command.env(
                    entry["name"].as_str().unwrap(),
                    entry["value"].as_str().unwrap(),
                );
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let lines = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let result: Value =
                serde_json::from_slice(&fs::read(directory.path().join("result.json")).unwrap())
                    .unwrap();
            assert_eq!(result["attempts"].as_u64().unwrap(), lines.len() as u64);
            assert_eq!(result["status"], if succeeds { "200" } else { "404" });
            assert_eq!(result["first_seconds"], "0.001");
            assert_eq!(result["total_seconds"], "0.002");
            assert_eq!(
                hex::decode(result["body_hex"].as_str().unwrap()).unwrap(),
                if succeeds {
                    b"test-body-200"
                } else {
                    b"test-body-404"
                }
            );
            if succeeds {
                assert_eq!(lines.len(), expected_attempts);
            } else {
                assert!(lines.len() >= 2);
                assert!(result["recovery_seconds"].as_u64().unwrap() >= 2);
            }
            for (index, attempt) in lines.iter().enumerate() {
                assert_eq!(attempt["attempt"], index + 1);
                assert!(attempt["recovery_seconds"].is_u64());
                assert_eq!(
                    attempt["status"],
                    if succeeds && index == lines.len() - 1 {
                        "200"
                    } else {
                        "404"
                    }
                );
            }
        }
    }

    #[test]
    fn startup_request_keeps_the_recovery_limit_and_packaging_identity() {
        let body = json!([{"value":{"id":crate::business_fixture::PACKAGING_ID}}]);
        let mut result = json!({"status":"200","first_seconds":"0.010","total_seconds":"0.020","recovery_seconds":120,"body_hex":hex::encode(serde_json::to_vec(&body).unwrap())});
        assert!((assert_response(&result, true).unwrap() - 20.0).abs() < f64::EPSILON);
        result["recovery_seconds"] = json!(121);
        assert!(assert_response(&result, true).is_err());
        result["recovery_seconds"] = json!(120);
        let keyed = json!([{"request_id":"startup-restart-first","value":{"id":crate::business_fixture::PACKAGING_ID}}]);
        let mut echoed = result.clone();
        echoed["body_hex"] = json!(hex::encode(serde_json::to_vec(&keyed).unwrap()));
        assert!(
            assert_response(&echoed, true).is_err(),
            "a read outcome carries no request identity"
        );
        result["status"] = json!("503");
        assert!(assert_response(&result, true).is_err());
    }

    #[test]
    fn restarting_a_replaced_or_unready_pod_does_not_pass() {
        let mut pod = json!({"metadata":{"uid":"same"},"status":{"containerStatuses":[{"name":"host","ready":true,"restartCount":1}]}});
        assert!(assert_host_restart(&pod, Some("same"), 1).is_ok());
        assert!(assert_host_restart(&pod, Some("different"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(2);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(1);
        pod["status"]["containerStatuses"][0]["ready"] = json!(false);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
    }
}
