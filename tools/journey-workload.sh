#!/usr/bin/env bash
# Journey workload-manifest render, shared by every application journey.
#
#   render_workload_manifest <spec-array-name> <out-file>
#
# LAW: reads only the array it is handed and the path it is told to write;
# touches no cluster; returns rather than exits.
#
# Required spec keys:
#   template              the checked-in workload example to render
#   namespace             the environment namespace to deploy into
#   image                 the built flow-http artifact reference
#   route_host            the setup-owned host this route answers on
#   tenant, environment, project, schema
#                         THIS application's identity claims. The template
#                         carries placeholders for them, so unlike the host
#                         overlay these ARE the harness's to substitute -- and
#                         a renderer that hardcoded them would deploy a second
#                         application under the first one's tenant, with route
#                         authorization resolving against the wrong data and
#                         the journey still green.
#   template_namespace, template_environment, template_image,
#   template_tenant, template_environment_value, template_project,
#   template_schema, template_interfaces
#                         what the template says TODAY. Naming the template's
#                         own text is what lets a drifted template fail loudly
#                         instead of passing its value through.
#
# EXACTLY N, NOT EXACTLY ONE. The host-values renderer demands each anchor fire
# exactly once, which is right for it. Here `namespace: wamn-system` appears
# twice by design -- the WorkloadDeployment and its nested spec -- so the count
# is declared per anchor. The generalisation came from the second consumer of
# this mechanism, which is the only way it would have been found: one caller
# cannot distinguish "exactly once" from "happens to occur once".
#
# The anchor machinery below is deliberately NOT shared with the host-values
# renderer. Each render carries bespoke rules -- an OTEL insertion there, a
# config block insertion here -- that are awk program text rather than data,
# and folding them into one function would mean passing program fragments as
# strings. Two copies of fifteen lines is the cheaper wrong. If a third
# renderer appears, unify them with `awk -f` and delete this paragraph.

render_workload_manifest() {
  local -n _rwm_spec=$1
  local out=$2
  local key anchor_map

  local required=(
    template namespace image route_host
    tenant environment project schema
    template_namespace template_environment template_image template_interfaces
    template_tenant template_environment_value template_project template_schema
  )
  for key in "${required[@]}"; do
    [[ -v _rwm_spec[$key] ]] || {
      echo "render_workload_manifest: spec is missing $key" >&2
      return 1
    }
  done
  [[ -f ${_rwm_spec[template]} ]] || {
    echo "render_workload_manifest: no template at ${_rwm_spec[template]}" >&2
    return 1
  }

  # anchor <TAB> replacement <TAB> expected-count
  anchor_map=$(printf '%s\t%s\t%s\n' \
    "namespace: ${_rwm_spec[template_namespace]}" "namespace: ${_rwm_spec[namespace]}" 2 \
    "environment: ${_rwm_spec[template_environment]}" "environment: ${_rwm_spec[namespace]}" 1 \
    "image: ${_rwm_spec[template_image]}" "image: ${_rwm_spec[image]}" 1 \
    "wamn.tenant: \"${_rwm_spec[template_tenant]}\"" "wamn.tenant: \"${_rwm_spec[tenant]}\"" 1 \
    "wamn.environment: \"${_rwm_spec[template_environment_value]}\"" "wamn.environment: \"${_rwm_spec[environment]}\"" 1 \
    "wamn.project: \"${_rwm_spec[template_project]}\"" "wamn.project: \"${_rwm_spec[project]}\"" 1 \
    "wamn.schema: \"${_rwm_spec[template_schema]}\"" "wamn.schema: \"${_rwm_spec[schema]}\"" 1)

  awk -v anchor_map="$anchor_map" -v route_host="${_rwm_spec[route_host]}" \
      -v interfaces_anchor="${_rwm_spec[template_interfaces]}" '
    BEGIN {
      rows = split(anchor_map, row, "\n")
      for (i = 1; i <= rows; i++) {
        if (row[i] == "") continue
        split(row[i], field, "\t")
        replacement[field[1]] = field[2]
        wanted[field[1]] = field[3] + 0
      }
    }
    {
      body = $0
      sub(/^[ \t]+/, "", body)
      if (body in replacement) {
        indent = substr($0, 1, length($0) - length(body))
        print indent replacement[body]
        fired[body]++
        next
      }
      if (body == interfaces_anchor) {
        print
        print "          config:"
        print "            host: " route_host
        fired[body]++
        next
      }
      print
    }
    END {
      for (anchor in wanted) {
        if (fired[anchor] == wanted[anchor]) continue
        printf "workload anchor matched %d times, want %d: [%s]\n", \
          fired[anchor], wanted[anchor], anchor > "/dev/stderr"
        wrong = 1
      }
      if (fired[interfaces_anchor] != 1) {
        printf "workload interfaces anchor matched %d times, want 1: [%s]\n", \
          fired[interfaces_anchor], interfaces_anchor > "/dev/stderr"
        wrong = 1
      }
      if (wrong) exit 1
    }
  ' "${_rwm_spec[template]}" >"$out" || return 1

  # The route host is setup-owned and must appear exactly once: a second
  # occurrence means the manifest answers on a host this journey did not claim.
  local hosts
  hosts=$(grep -Fc -- "${_rwm_spec[route_host]}" "$out" || true)
  [[ $hosts -eq 1 ]] || {
    echo "rendered workload carries $hosts occurrences of the route host, want 1" >&2
    return 1
  }
}
