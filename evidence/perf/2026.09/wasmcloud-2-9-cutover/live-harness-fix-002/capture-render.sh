#!/usr/bin/env bash
# Proof for tools/journey-workload.sh. No cluster: a text transform over one
# checked-in template.
set -euo pipefail

repo_root=${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$repo_root/tools/journey-workload.sh"
work=${2:?render output directory is required}; mkdir -p "$work"
failures=0
TEMPLATE=$repo_root/deploy/platform/http-route-workload.example.yaml

check() {
  if [[ $2 -eq 0 ]]; then printf '  ok    %s\n' "$1"
  else printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); fi
}
refused_saying() { local err; err=$("$@" 2>&1 >/dev/null) && return 1; [[ $err == *"$REASON"* ]]; }

# Deliberately unguessable, so a hardcoded stand-in cannot match.
build_spec() { # <array-name> <template> <tenant> <project> <schema> <environment> <catalog>
  eval "declare -gA $1=(
    [template]=$2
    [namespace]=warehouse-eu-3
    [image]=registry.probe.invalid:5000/proofs/flow-http@sha256:abc123
    [route_host]=probe.route.invalid
    [tenant]=$3 [environment]=$6 [project]=$4 [schema]=$5 [catalog]=$7
    [template_namespace]=wamn-system [template_environment]=wamn-system
    [template_image]=registry.wamn-system.svc.cluster.local:5000/wamn/flow-http:dev
    [template_interfaces]='interfaces: [incoming-handler]'
    [template_tenant]=00000000-0000-0000-0000-000000000001 [template_catalog]=default
    [template_environment_value]=poc [template_project]=default [template_schema]=public
  )"
}


build_spec rcv "$TEMPLATE" receiving-route-auth receiving receiving dev default
render_workload_manifest rcv "$work/rcv.yaml"
build_spec wms "$TEMPLATE" wms-route-auth wms wms dev wms-catalog
render_workload_manifest wms "$work/wms.yaml"
