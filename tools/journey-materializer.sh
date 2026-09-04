#!/usr/bin/env bash
# Journey materializer-manifest render, shared by every application journey.
#
#   render_materializer_manifest <spec-array-name> <out-file>
#
# LAW: reads only the array it is handed and the path it is told to write;
# touches no cluster; returns rather than exits.
#
# WHY THIS ONE SUBSTITUTES WHERE THE HOST OVERLAY ASSERTS. The overlay is a
# per-application file carrying that application's own identity claims, so a
# harness that rewrote them would be forging the thing it is supposed to check.
# deploy/platform/materializer.example.yaml is the opposite: a GENERIC example
# whose identity fields are placeholders (materializer-demo, t1, default, menv,
# morg, mproj, EVT_morg_menv) that exist to be filled in. Substituting them is
# the harness's job. What is NOT the harness's job is deciding what to fill
# them WITH -- a renderer holding one application's values as literals deploys
# the second application under the first one's tenant, its reads resolve
# against the wrong catalog rows, and the journey goes green. That is the same
# hazard the workload renderer had, in a third file.
#
# Required spec keys -- what THIS application declares:
#   template              the checked-in materializer example to render
#   workload              the WorkloadDeployment name to render as
#   namespace             the environment namespace to deploy into
#   image                 the pushed materializer artifact reference
#   tenant, project, environment
#                         the host-injected identity claims; these scope the
#                         tenant-qualified catalog reads and select the
#                         per-project pool
#   event_stream          the JetStream stream this application's events land
#                         on -- platform-named EVT_<org>_<env>, so a literal
#                         here subscribes a second application to the first
#                         one's events
#   org                   the organisation the subscription config names
#   fetch_ms, sweep_ms    the guest loop's poll intervals. Journey tuning, not
#                         identity, and named here for the same reason: a
#                         value the harness holds is a value the caller cannot
#                         see it holding.
#
# Required spec keys -- what the TEMPLATE says today:
#   template_name, template_system_namespace, template_image, template_tenant,
#   template_config_project, template_environment, template_stream,
#   template_org, template_env_project
#                         Naming the template's own text is what lets a drifted
#                         template fail loudly instead of passing its
#                         placeholder through into a deployed workload.
#
# EXACTLY ONCE, and here that is a property of the template rather than a
# choice. Every anchor below occurs once in materializer.example.yaml, and the
# template is generic -- both applications render the same file -- so the
# counts cannot differ per caller the way the workload template's nested
# namespace does. If a future template repeats one, this rule reports it
# instead of silently rewriting the first occurrence.

render_materializer_manifest() {
  local -n _rmm_spec=$1
  local out=$2
  local key anchor_map tenant_env_anchor

  local required=(
    template workload namespace image
    tenant project environment event_stream org fetch_ms sweep_ms
    template_name template_system_namespace template_image template_tenant
    template_config_project template_environment template_stream
    template_org template_env_project
  )
  for key in "${required[@]}"; do
    [[ -v _rmm_spec[$key] ]] || {
      echo "render_materializer_manifest: spec is missing $key" >&2
      return 1
    }
  done
  [[ -f ${_rmm_spec[template]} ]] || {
    echo "render_materializer_manifest: no template at ${_rmm_spec[template]}" >&2
    return 1
  }

  # The WAMN_MAT_TENANT line is both a substitution and the insertion point for
  # the two poll intervals, so it is handled apart from the plain anchor map.
  tenant_env_anchor="WAMN_MAT_TENANT: ${_rmm_spec[template_tenant]}"

  # anchor <TAB> replacement
  anchor_map=$(printf '%s\t%s\n' \
    "name: ${_rmm_spec[template_name]}" "name: ${_rmm_spec[workload]}" \
    "namespace: ${_rmm_spec[template_system_namespace]}" "namespace: ${_rmm_spec[namespace]}" \
    "environment: ${_rmm_spec[template_system_namespace]}" "environment: ${_rmm_spec[namespace]}" \
    "image: ${_rmm_spec[template_image]}" "image: ${_rmm_spec[image]}" \
    "wamn.tenant: ${_rmm_spec[template_tenant]}" "wamn.tenant: ${_rmm_spec[tenant]}" \
    "wamn.project: ${_rmm_spec[template_config_project]}" "wamn.project: ${_rmm_spec[project]}" \
    "wamn.environment: ${_rmm_spec[template_environment]}" "wamn.environment: ${_rmm_spec[environment]}" \
    "WAMN_MAT_STREAM: ${_rmm_spec[template_stream]}" "WAMN_MAT_STREAM: ${_rmm_spec[event_stream]}" \
    "WAMN_MAT_ORG: ${_rmm_spec[template_org]}" "WAMN_MAT_ORG: ${_rmm_spec[org]}" \
    "WAMN_MAT_PROJECT: ${_rmm_spec[template_env_project]}" "WAMN_MAT_PROJECT: ${_rmm_spec[project]}" \
    "WAMN_MAT_ENV: ${_rmm_spec[template_environment]}" "WAMN_MAT_ENV: ${_rmm_spec[environment]}")

  awk -v anchor_map="$anchor_map" -v tenant_env_anchor="$tenant_env_anchor" \
      -v tenant="${_rmm_spec[tenant]}" -v fetch_ms="${_rmm_spec[fetch_ms]}" \
      -v sweep_ms="${_rmm_spec[sweep_ms]}" '
    BEGIN {
      rows = split(anchor_map, row, "\n")
      for (i = 1; i <= rows; i++) {
        if (row[i] == "") continue
        split(row[i], field, "\t")
        replacement[field[1]] = field[2]
      }
    }
    {
      body = $0
      sub(/^[ \t]+/, "", body)
      indent = substr($0, 1, length($0) - length(body))
      if (body == tenant_env_anchor) {
        print indent "WAMN_MAT_TENANT: " tenant
        print indent "WAMN_MAT_FETCH_MS: \"" fetch_ms "\""
        print indent "WAMN_MAT_SWEEP_MS: \"" sweep_ms "\""
        fired[body]++
        next
      }
      if (body in replacement) {
        print indent replacement[body]
        fired[body]++
        next
      }
      print
    }
    END {
      for (anchor in replacement) {
        if (fired[anchor] == 1) continue
        printf "materializer anchor matched %d times, want 1: [%s]\n", \
          fired[anchor], anchor > "/dev/stderr"
        wrong = 1
      }
      if (fired[tenant_env_anchor] != 1) {
        printf "materializer tenant-env anchor matched %d times, want 1: [%s]\n", \
          fired[tenant_env_anchor], tenant_env_anchor > "/dev/stderr"
        wrong = 1
      }
      if (wrong) exit 1
    }
  ' "${_rmm_spec[template]}" >"$out" || return 1

  # No placeholder may survive into a manifest the operator will accept. The
  # anchor counts above prove each REPLACEMENT fired; this proves the template
  # carried no SECOND copy of a placeholder under an anchor nobody declared --
  # a comment naming the demo tenant, a field added upstream. The two are
  # different questions and the second is the one that reaches the cluster.
  local placeholder survivors
  for placeholder in "${_rmm_spec[template_name]}" "${_rmm_spec[template_tenant]}" \
      "${_rmm_spec[template_stream]}" "${_rmm_spec[template_org]}" \
      "${_rmm_spec[template_env_project]}"; do
    survivors=$(grep -Fc -- "$placeholder" "$out" || true)
    [[ $survivors -eq 0 ]] || {
      echo "rendered materializer still carries the template placeholder [$placeholder] $survivors times" >&2
      return 1
    }
  done
}
