#!/usr/bin/env bash
# Journey throughput-step Job render, shared by every application journey.
#
#   render_throughput_job <spec-array-name> <out-file>
#
# LAW: reads only the array it is handed and the path it is told to write;
# touches no cluster; returns rather than exits.
#
# THE LOAD GENERATOR IS A STOCK TOOL IN A POD, PINNED BY DIGEST (owner ruling,
# wamn-0h0g.17.27): oha for the HTTP layers, pgbench from the pinned postgres
# image for the direct-PostgreSQL layer. Nothing here generates load; this
# renders the Job that runs the tool for one (layer, concurrency) step and
# prints the tool's own output, which the Rust half reads back. The manifest
# is JSON, built by jq from the spec's values, so every value crosses as a
# JSON string and the proof can assert the render with jq alone.
#
# oha runs WITHOUT A SHELL: the image's entrypoint is /bin/oha and its args are
# a list. The route-caller PAT reaches it through Kubernetes' own $(VAR)
# expansion of container args, which needs no shell and never lands the token
# in the manifest. pgbench runs under /bin/sh so its custom script can be
# written to disk and its sampled per-transaction log printed after the
# summary, behind one marker line the reader splits on.
#
# Required spec keys, every layer:
#   job, namespace          the Job's name and the environment namespace
#   layer                   route | nodb | pg
#   concurrency             the client count for this step, an integer
#   duration_seconds        the step's fixed duration, an integer
# route (oha, POST, authenticated):
#   oha_image, url, route_host, secret_name, body
# nodb (oha, GET, unauthenticated -- the route the guest answers 404 to):
#   oha_image, url, route_host
# pg (pgbench, the same statement from a direct client):
#   pgbench_image, pg_host, pg_port, pg_database, pg_user, pg_password,
#   sql, sampling_rate

# The marker the pgbench Job prints between its summary and its log lines.
# Pinned by wamn_proof_integration::throughput_bench::PGBENCH_LOG_MARKER.
readonly WAMN_THROUGHPUT_PGBENCH_LOG_MARKER='===LOGS==='

render_throughput_job() {
  local -n _rtj_spec=$1
  local out=$2
  local key
  local -a required=(job namespace layer concurrency duration_seconds)

  for key in "${required[@]}"; do
    [[ -v _rtj_spec[$key] ]] || {
      echo "render_throughput_job: spec is missing $key" >&2
      return 1
    }
  done
  case "${_rtj_spec[layer]}" in
    route) required=(oha_image url route_host secret_name body) ;;
    nodb) required=(oha_image url route_host) ;;
    pg) required=(pgbench_image pg_host pg_port pg_database pg_user pg_password sql sampling_rate) ;;
    *)
      echo "render_throughput_job: unknown layer ${_rtj_spec[layer]} (route, nodb or pg)" >&2
      return 1
      ;;
  esac
  for key in "${required[@]}"; do
    [[ -v _rtj_spec[$key] ]] || {
      echo "render_throughput_job: ${_rtj_spec[layer]} spec is missing $key" >&2
      return 1
    }
  done
  for key in concurrency duration_seconds; do
    [[ ${_rtj_spec[$key]} =~ ^[1-9][0-9]*$ ]] || {
      echo "render_throughput_job: $key must be a positive integer, got '${_rtj_spec[$key]}'" >&2
      return 1
    }
  done
  # A digest pin is the whole point of the ruling; a tag would move under us.
  for key in oha_image pgbench_image; do
    [[ -v _rtj_spec[$key] ]] || continue
    [[ ${_rtj_spec[$key]} == *@sha256:* ]] || {
      echo "render_throughput_job: $key must be pinned by digest, got '${_rtj_spec[$key]}'" >&2
      return 1
    }
  done

  local deadline=$(( _rtj_spec[duration_seconds] * 3 + 90 ))
  case "${_rtj_spec[layer]}" in
    route)
      jq -n \
        --arg job "${_rtj_spec[job]}" --arg namespace "${_rtj_spec[namespace]}" \
        --arg layer "${_rtj_spec[layer]}" --arg image "${_rtj_spec[oha_image]}" \
        --arg c "${_rtj_spec[concurrency]}" --arg z "${_rtj_spec[duration_seconds]}s" \
        --arg url "${_rtj_spec[url]}" --arg host "${_rtj_spec[route_host]}" \
        --arg secret "${_rtj_spec[secret_name]}" --arg body "${_rtj_spec[body]}" \
        --argjson deadline "$deadline" '
        {
          apiVersion: "batch/v1", kind: "Job",
          metadata: { name: $job, namespace: $namespace,
                      labels: { "wamn.bench/layer": $layer, "wamn.bench/concurrency": $c } },
          spec: {
            activeDeadlineSeconds: $deadline, backoffLimit: 0,
            template: { spec: {
              restartPolicy: "Never",
              containers: [ {
                name: "oha", image: $image, imagePullPolicy: "IfNotPresent",
                env: [ { name: "ROUTE_CALLER_PAT",
                         valueFrom: { secretKeyRef: { name: $secret, key: "token" } } } ],
                command: ["/bin/oha"],
                args: [ "--no-tui", "-z", $z, "-c", $c, "--output-format", "json",
                        "-m", "POST",
                        "-H", ("Host: " + $host),
                        "-H", "Content-Type: application/json",
                        "-H", "Authorization: Bearer $(ROUTE_CALLER_PAT)",
                        "-d", $body,
                        $url ]
              } ]
            } }
          }
        }' >"$out"
      ;;
    nodb)
      jq -n \
        --arg job "${_rtj_spec[job]}" --arg namespace "${_rtj_spec[namespace]}" \
        --arg layer "${_rtj_spec[layer]}" --arg image "${_rtj_spec[oha_image]}" \
        --arg c "${_rtj_spec[concurrency]}" --arg z "${_rtj_spec[duration_seconds]}s" \
        --arg url "${_rtj_spec[url]}" --arg host "${_rtj_spec[route_host]}" \
        --argjson deadline "$deadline" '
        {
          apiVersion: "batch/v1", kind: "Job",
          metadata: { name: $job, namespace: $namespace,
                      labels: { "wamn.bench/layer": $layer, "wamn.bench/concurrency": $c } },
          spec: {
            activeDeadlineSeconds: $deadline, backoffLimit: 0,
            template: { spec: {
              restartPolicy: "Never",
              containers: [ {
                name: "oha", image: $image, imagePullPolicy: "IfNotPresent",
                command: ["/bin/oha"],
                args: [ "--no-tui", "-z", $z, "-c", $c, "--output-format", "json",
                        "-m", "GET",
                        "-H", ("Host: " + $host),
                        $url ]
              } ]
            } }
          }
        }' >"$out"
      ;;
    pg)
      # pgbench threads are capped at eight: past that the generator, not the
      # server, is what the extra threads measure on an eight-core box.
      local threads=${_rtj_spec[concurrency]}
      (( threads > 8 )) && threads=8
      local script
      script=$(printf '%s\n' \
        "cat >/tmp/bench.sql <<'SQL'" \
        "${_rtj_spec[sql]}" \
        "SQL" \
        "pgbench -h ${_rtj_spec[pg_host]} -p ${_rtj_spec[pg_port]} -U ${_rtj_spec[pg_user]} -d ${_rtj_spec[pg_database]} \\" \
        "  -n -M prepared -c ${_rtj_spec[concurrency]} -j $threads -T ${_rtj_spec[duration_seconds]} \\" \
        "  -f /tmp/bench.sql -l --log-prefix /tmp/pgb --sampling-rate ${_rtj_spec[sampling_rate]}" \
        "echo $WAMN_THROUGHPUT_PGBENCH_LOG_MARKER" \
        "cat /tmp/pgb*")
      jq -n \
        --arg job "${_rtj_spec[job]}" --arg namespace "${_rtj_spec[namespace]}" \
        --arg layer "${_rtj_spec[layer]}" --arg image "${_rtj_spec[pgbench_image]}" \
        --arg c "${_rtj_spec[concurrency]}" --arg password "${_rtj_spec[pg_password]}" \
        --arg script "$script" --argjson deadline "$deadline" '
        {
          apiVersion: "batch/v1", kind: "Job",
          metadata: { name: $job, namespace: $namespace,
                      labels: { "wamn.bench/layer": $layer, "wamn.bench/concurrency": $c } },
          spec: {
            activeDeadlineSeconds: $deadline, backoffLimit: 0,
            template: { spec: {
              restartPolicy: "Never",
              containers: [ {
                name: "pgbench", image: $image, imagePullPolicy: "IfNotPresent",
                env: [ { name: "PGPASSWORD", value: $password } ],
                command: ["/bin/sh", "-ec"],
                args: [ $script ]
              } ]
            } }
          }
        }' >"$out"
      ;;
  esac
}
