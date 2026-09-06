#!/usr/bin/env bash
# Journey startup-probe Job render, shared by every application journey.
#
#   render_probe_job <spec-array-name> <out-file>
#
# LAW: reads only the array it is handed and the path it is told to write;
# touches no cluster; returns rather than exits.
#
# A PROBE IS THREE THINGS -- a route, a body, and what the answer must contain.
# The third lives with the caller, because it is a jq predicate over the
# response rather than anything this render emits; the first two are here. A
# harness holding any of them deploys one application and measures another's
# route.
#
# THE BODY IS A JSON OBJECT, NOT A FRAGMENT. The request_id belongs to the
# phase, the rest belongs to the application, and merging them with jq means a
# malformed declaration fails on the developer's machine at render time rather
# than as a 400 inside a Job forty minutes into a cluster run. jq preserves
# left-then-right key order, so request_id stays first and the rendered body is
# byte-for-byte what the hardcoded literal produced.
#
# Required spec keys:
#   job, phase            the Job's name and the measurement phase it reports
#   request_id            the phase's correlation id, merged into the body
#   namespace             the environment namespace the route answers in
#   host_image            the image carrying curl; kind-loaded, never pulled
#   secret_name           the Secret holding the route-caller PAT
#   route_host            the Host header the route is published under
#   trace_id, parent_span_id
#                         the pinned traceparent this phase correlates on
#   route                 THIS application's probe path
#   body                  THIS application's probe payload, a JSON object
#
# THE THREE TIMINGS ARE NOT PARAMETERS. The 150-second retry window, the Job's
# 200-second activeDeadlineSeconds and the caller's 120-second recovery ceiling
# are a set -- the window must fit inside the deadline with room for the final
# report, and must exceed the ceiling so a breach is REPORTED as a number rather
# than as a timeout. No application has a reason to want a different one.
#
# WHY 150 AND NOT 45. Measured recovery after a host restart is 89 seconds
# (docs/perf/2026.09/2b-503-retryable.md): the operator rebinds the workload on
# its heartbeat timeout, and until it does, the route is unbound. The old
# 45-second window gave up 44 seconds early, which is why this arm read as a
# product failure for six consecutive runs when the route was in fact returning.
# The ceiling that decides pass or fail lives at the caller, next to the
# assertion it drives.

render_probe_job() {
  local -n _rpj_spec=$1
  local out=$2
  local key probe_body

  local required=(
    job phase request_id namespace host_image secret_name route_host
    trace_id parent_span_id route body
  )
  for key in "${required[@]}"; do
    [[ -v _rpj_spec[$key] ]] || {
      echo "render_probe_job: spec is missing $key" >&2
      return 1
    }
  done

  probe_body=$(jq -cn --arg request_id "${_rpj_spec[request_id]}" \
    --argjson fields "${_rpj_spec[body]}" '[{request_id: $request_id} + $fields]') || {
    echo "render_probe_job: body is not a JSON object: ${_rpj_spec[body]}" >&2
    return 1
  }
  # The body lands inside a single-quoted shell literal in the Job's script.
  # A single quote in the declaration would close it and the rest would be
  # read as shell -- so refuse here, where the message names the cause.
  [[ $probe_body != *"'"* ]] || {
    echo "render_probe_job: body carries a single quote and cannot ride the probe's --data literal" >&2
    return 1
  }
  cat >"$out" <<EOF
apiVersion: batch/v1
kind: Job
metadata:
  name: ${_rpj_spec[job]}
  namespace: ${_rpj_spec[namespace]}
spec:
  activeDeadlineSeconds: 200
  backoffLimit: 0
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: probe
          image: ${_rpj_spec[host_image]}
          imagePullPolicy: Never
          env:
            - name: ROUTE_CALLER_PAT
              valueFrom:
                secretKeyRef:
                  name: ${_rpj_spec[secret_name]}
                  key: token
          command: ["/bin/sh", "-ec"]
          args:
            - |
              # BOUNDED RETRY, reporting EVERY attempt. The question this
              # answers is product-versus-journey: a route that returns 404
              # immediately after a host restart and then recovers means
              # Kubernetes readiness precedes in-host route registration and
              # this journey waited on the wrong signal; one that never
              # recovers means released routes are lost across a restart. A
              # single attempt cannot tell those apart, and every attempt's
              # status and elapsed time is the measurement.
              probe_start=\$(date +%s)
              attempt=0
              while :; do
                attempt=\$((attempt + 1))
                metrics=\$(curl --silent --show-error --connect-timeout 5 --max-time 60 \\
                  --output /tmp/body --write-out '%{http_code} %{time_starttransfer} %{time_total}' \\
                  --header 'Host: ${_rpj_spec[route_host]}' \\
                  --header 'Content-Type: application/json' \\
                  --header "Authorization: Bearer \$ROUTE_CALLER_PAT" \\
                  --header 'traceparent: 00-${_rpj_spec[trace_id]}-${_rpj_spec[parent_span_id]}-01' \\
                  --data '$probe_body' \\
                  'http://flow-http.${_rpj_spec[namespace]}.svc.cluster.local${_rpj_spec[route]}') || metrics='000 0 0'
                set -- \$metrics
                probe_elapsed=\$(( \$(date +%s) - probe_start ))
                printf '%s\\n' \\
                  "JOURNEY_STARTUP_ATTEMPT phase=${_rpj_spec[phase]} attempt=\$attempt status=\$1 since_restart_seconds=\$probe_elapsed time_total_seconds=\$3"
                [ "\$1" = 200 ] && break
                [ "\$probe_elapsed" -ge 150 ] && break
                sleep 1
              done
              # REPORT BEFORE ASSERTING. The shell is 'sh -ec', so a failing
              # 'test' exits here and anything printed after it is never
              # printed at all -- which made a non-200 probe silent by
              # construction: exit 1, an empty log, and no way to tell a 503
              # from a timeout without another cluster.
              # APOSTROPHES, NOT BACKTICKS. This heredoc's delimiter is
              # unquoted, so a backtick is command substitution run by the
              # LOCAL shell at render time. The first draft of these two lines
              # quoted the shell name that way; rendering ran it on this
              # machine and substituted its empty output, deleting the
              # sentence from every manifest the journey wrote.
              printf '%s\n' \\
                "JOURNEY_STARTUP_REQUEST phase=${_rpj_spec[phase]} status=\$1 time_starttransfer_seconds=\$2 time_total_seconds=\$3 recovery_seconds=\$probe_elapsed"
              printf 'JOURNEY_STARTUP_BODY '
              tr -d '\n' </tmp/body
              printf '\n'
              test "\$1" = 200
EOF
}
