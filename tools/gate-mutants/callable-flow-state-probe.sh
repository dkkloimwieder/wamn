#!/usr/bin/env bash
set -euo pipefail

namespace=wamn-system
while (($#)); do
  case "$1" in
    --namespace) namespace=${2-}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

kubectl_executable=${KUBECTL:-kubectl}
postgres_pod="$($kubectl_executable -n "$namespace" get pods -l app=postgres \
  -o jsonpath='{.items[0].metadata.name}')"
if [[ -z "$postgres_pod" ]]; then
  echo "fixture PostgreSQL pod not found" >&2
  exit 2
fi

printf 'runner-replicas='
$kubectl_executable -n "$namespace" get deployment runner \
  -o jsonpath='{.spec.replicas}{"\n"}'

$kubectl_executable -n "$namespace" exec "$postgres_pod" -- \
  psql -X -qAt -U postgres -d postgres -v ON_ERROR_STOP=1 -c \
  "SELECT 'f4-database=' || count(*) FROM pg_database WHERE datname='wamn_f4proof';
   SELECT 'f4-role=' || count(*) FROM pg_roles WHERE rolname LIKE 'wamn_cdc_%f4%';"

$kubectl_executable -n "$namespace" exec "$postgres_pod" -- \
  psql -X -qAt -U postgres -d wamn -v ON_ERROR_STOP=1 -c \
  "SELECT 'schema=' || count(*) FROM pg_namespace WHERE nspname='wamn_callable_flow_schema';
   SELECT 'cron-runs=' || count(*) FROM wamn_run.runs
     WHERE tenant_id='callable-cron-gate-v2' AND flow_id='callable-cron-flow';
   SELECT 'cron-queue=' || count(*) FROM wamn_run.run_queue
     WHERE tenant_id='callable-cron-gate-v2';
   SELECT 'cron-activation=' || coalesce(string_agg(
       attachment_id || ':' || confirmed_definition_hash || ':' || enabled::text,
       ',' ORDER BY attachment_id), '')
     FROM catalog.attachment_activation
     WHERE tenant_id='callable-cron-gate-v2' AND catalog_id='callable-cron'
       AND environment='gate';
   SELECT 'f3-runs=' || count(*) FROM wamn_run.runs
     WHERE tenant_id='demo-tenant' AND flow_id='escalate-stale-holds';
   SELECT 'f3-queue=' || count(*) FROM wamn_run.run_queue q
     WHERE q.tenant_id='demo-tenant' AND EXISTS (
       SELECT 1 FROM wamn_run.runs r
       WHERE r.tenant_id=q.tenant_id AND r.run_id=q.run_id
         AND r.flow_id='escalate-stale-holds');"
