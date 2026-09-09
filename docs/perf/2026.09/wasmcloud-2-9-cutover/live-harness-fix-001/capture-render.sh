#!/usr/bin/env bash
# Proof for tools/journey-host-values.sh. Runs nothing against a cluster: the
# renderer is a pure text transform over two checked-in templates, so every
# check here is local, offline, and takes under a second.
#
#   tools/journey-host-values-proof [repo_root]
#
# Three properties, each of which has caught a real defect:
#
#   1. BYTE IDENTITY. Receiving renders exactly the bytes it rendered before
#      the block was un-fused and lifted. Both arms -- the normal one and
#      --measure-startup, which differ only in replica count.
#
#   2. SECOND CONSUMER. WMS renders through the same function with a different
#      identity, and every derived secret anchor fires against its own name.
#      A constant is only proven generic when a second consumer with different
#      values passes through it.
#
#   3. NAMEREF BINDING. A global holding decoys is planted under the callee's
#      nameref name and a caller passes a local array of that name. Bash
#      resolves such a cycle to the global with only a WARNING, so the render
#      would silently use the decoys. Plausible caller names must all bind to
#      the caller; the internal name itself is the control and MUST collide,
#      because a control that passes means the check is inert.
set -euo pipefail

repo_root=${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$repo_root/tools/journey-host-values.sh"
work=${2:?render output directory is required}; mkdir -p "$work"
failures=0

families=(executor-platform identity-reader http-admitter event-materializer)
common="
  [host_base_template]=$repo_root/deploy/platform/values-host-default.yaml
  [template_tag]=dev [template_replicas]=3 [template_namespace]=wamn-system
  [role_families]='${families[*]}'
  [guest_secret_anchor]=wamn-host-db
  [namespace]=warehouse-eu-3 [host_tag]=sha-9f3c1ab
  [component_artifact_base]=registry.probe.invalid:5000/proofs/components
  [release_artifact_base]=registry.probe.invalid:5000/proofs/releases
  [manifest_digest]=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
  [nats_url]=nats://nats.probe.invalid:4222
  [guest_secret_name]=probe-guest-sql
  [secret_name:executor-platform]=probe-executor-platform
  [secret_name:identity-reader]=probe-identity-reader
  [secret_name:http-admitter]=probe-http-admitter
  [secret_name:event-materializer]=probe-event-materializer
  [object_store_secret_name]=
"
# Values chosen so a hardcoded stand-in cannot match them by coincidence.
render_app() { # <out> <replicas> <org> <project> <env> <overlay> [object-store secret name]
  local out=$1 replicas=$2
  eval "local -A app_spec=(
    $common
    [org]=$3 [project]=$4 [environment]=$5
    [host_values_overlay]=$repo_root/$6
    [object_store_secret_name]=${7:-}
  )"
  render_host_values app_spec "$out" "$replicas"
}



RCV=deploy/platform/values-host-receiving-pat.yaml
for arm in normal:3 measure:0; do
  mkdir -p "$work/${arm%%:*}"
  render_app "$work/${arm%%:*}" "${arm#*:}" acme receiving dev "$RCV"
done
