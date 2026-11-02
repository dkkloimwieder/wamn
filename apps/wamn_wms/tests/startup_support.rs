//! Request and restart objects shared by the local and deployed checks.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_test_infrastructure::workload::HTTP_PROBE_IMAGE;

pub(crate) fn request_job(
    namespace: &str,
    service: &str,
    route_host: &str,
    secret: &str,
    job: &str,
    trace: (&str, &str),
    query: &str,
) -> Value {
    let (trace_id, parent_span) = trace;
    // The mounted PAT stays inside the owned request pod. Results contain no credentials.
    let script = r#"probe_start=$(date +%s)
attempt=0
while :; do
  attempt=$((attempt + 1))
  metrics=$(curl --silent --show-error --connect-timeout 5 --max-time 60 \
    --output /tmp/body --write-out '%{http_code} %{time_starttransfer} %{time_total}' \
    --header "Host: $ROUTE_HOST" \
    --header "Authorization: Bearer $OPERATOR_PAT" --header "traceparent: $TRACEPARENT" \
    "$ROUTE_URL") || metrics='000 0 0'
  set -- $metrics
  elapsed=$(( $(date +%s) - probe_start ))
  printf '{"attempt":%s,"status":"%s","recovery_seconds":%s,"total_seconds":"%s"}\n' "$attempt" "$1" "$elapsed" "$3"
  [ "$1" = 200 ] && break
  [ "$elapsed" -ge 150 ] && break
  sleep 1
done
body=$(od -An -v -tx1 /tmp/body | tr -d ' \n')
printf '{"status":"%s","first_seconds":"%s","total_seconds":"%s","recovery_seconds":%s,"attempts":%s,"body_hex":"%s"}\n' "$1" "$2" "$3" "$elapsed" "$attempt" "$body" >/dev/termination-log
test "$1" = 200
"#;
    json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":job,"namespace":namespace},"spec":{"activeDeadlineSeconds":200,"backoffLimit":0,"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"probe","image":HTTP_PROBE_IMAGE,"imagePullPolicy":"IfNotPresent","terminationMessagePolicy":"File","command":["/bin/sh","-ec"],"args":[script],"env":[
        {"name":"OPERATOR_PAT","valueFrom":{"secretKeyRef":{"name":secret,"key":"token"}}},
        {"name":"ROUTE_HOST","value":route_host},{"name":"TRACEPARENT","value":format!("00-{trace_id}-{parent_span}-01")},
        {"name":"ROUTE_URL","value":format!("http://{service}.{namespace}.svc.cluster.local/packaging/get?{query}")}
    ]}]}}}})
}

pub(crate) fn assert_response(result: &Value, restarted: bool) -> anyhow::Result<f64> {
    ensure!(
        result["status"] == "200",
        "startup request must return HTTP 200"
    );
    let seconds = |name: &str| -> anyhow::Result<f64> {
        let text = result[name].as_str().context("request timing is text")?;
        let (whole, fraction) = text
            .split_once('.')
            .context("request timing has a decimal point")?;
        ensure!(
            !whole.is_empty()
                && !fraction.is_empty()
                && whole
                    .bytes()
                    .chain(fraction.bytes())
                    .all(|byte| byte.is_ascii_digit()),
            "request timing is a nonnegative decimal"
        );
        let value: f64 = text.parse()?;
        ensure!(value.is_finite(), "request timing is finite");
        Ok(value)
    };
    seconds("first_seconds")?;
    let total = seconds("total_seconds")?;
    if restarted {
        ensure!(
            result["recovery_seconds"]
                .as_u64()
                .context("recovery is a whole number of seconds")?
                <= 120,
            "request recovery exceeded 120 seconds"
        );
    }
    let body: Value = serde_json::from_slice(&hex::decode(
        result["body_hex"]
            .as_str()
            .context("startup response has bytes")?,
    )?)?;
    let rows = body.as_array().context("startup response is an array")?;
    ensure!(
        rows.len() == 1
            && rows[0].get("request_id").is_none()
            && rows[0].get("error").is_none()
            && rows[0]["value"]["id"] == crate::business_fixture::PACKAGING_ID,
        "startup response must return the requested WMS packaging without an error"
    );
    Ok(total * 1000.0)
}

pub(crate) fn restart_trace_is_complete(document: &Value) -> bool {
    let spans = document["resourceSpans"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|batch| batch["scopeSpans"].as_array().into_iter().flatten())
        .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    let count = |name: &str| spans.iter().filter(|span| span["name"] == name).count();
    count("wamn.component.invoke") == 1
        && count("wamn.postgres.statement") == 1
        && [
            "wamn.component.pull",
            "wamn.component.compile",
            "wamn.component.linker_setup",
            "wamn.component.link",
            "load_component_bytes",
            "resolve_workload",
            "link_components",
        ]
        .iter()
        .all(|name| count(name) == 0)
}

pub(crate) fn assert_host_restart(
    pod: &Value,
    uid: Option<&str>,
    restarts: u64,
) -> anyhow::Result<()> {
    ensure!(
        uid.is_none_or(|uid| pod["metadata"]["uid"] == uid),
        "host restart replaced the measured pod"
    );
    ensure!(
        pod["status"]["containerStatuses"]
            .as_array()
            .is_some_and(
                |statuses| statuses.iter().any(|status| status["name"] == "host"
                    && status["ready"] == true
                    && status["restartCount"].as_u64() == Some(restarts))
            ),
        "measured host must be ready with the exact restart count"
    );
    Ok(())
}
