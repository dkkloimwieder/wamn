#!/usr/bin/env bash
# Journey host-values renderer, shared by every application journey.
#
#   render_host_values <spec-array-name> <out-dir>
#
# LAW: this function reads NOTHING but the array it is handed and the output
# directory. It reaches no global, consults no environment, and knows no
# application. Everything it needs is a key, and a missing key is an error
# rather than an empty string, so a caller that forgets one is told.
#
# Required keys:
#   repo-relative or absolute template paths
#     host_base_template        the shared base values file
#     host_values_overlay       THIS application's overlay, per-app by ruling
#   the template's own current text, so drift fails loudly rather than passing
#     template_tag              what the base file says its tag is
#     template_replicas         what both files say their replica count is
#     template_namespace        what both files say their namespace is
#   the application's identity, from which every secret anchor is derived
#     org, project, environment
#     role_families             space-separated; each yields one derived anchor
#     guest_secret_anchor       the App family's PLATFORM-fixed anchor
#   the run's own values
#     namespace, host_tag, component_artifact_base, release_artifact_base
#     manifest_digest, nats_url, guest_secret_name
#     secret_name:<family>      one per declared family
#
# Positional: $2 is the directory to write host-base.yaml and host-overlay.yaml
# into. The third argument is the replica count to RENDER -- already decided by
# the caller's arm, because that number and the count asserted after deployment
# are two different facts that once shared one name.

render_host_values() {
  # The nameref is deliberately ugly. A nameref that shares a name with the
  # caller's own variable resolves to the wrong one and bash only WARNS, so a
  # caller who named their array "spec" would render from whatever else was in
  # scope. The contract is a parameter; the name must not be guessable.
  local -n _rhv_spec=$1
  local out_dir=$2
  local render_replicas=$3
  # Nothing this function computes escapes it. The two output paths are
  # deterministic from out_dir, so the caller names them itself rather than
  # receiving them through a global.
  local family host_base host_overlay host_base_anchor_map host_overlay_anchor_map

  local required=(
    host_base_template host_values_overlay
    template_tag template_replicas template_namespace
    org project environment role_families guest_secret_anchor
    namespace host_tag component_artifact_base release_artifact_base
    manifest_digest nats_url guest_secret_name
  )
  local key
  for key in "${required[@]}"; do
    [[ -v _rhv_spec[$key] ]] || {
      echo "render_host_values: spec is missing $key" >&2
      return 1
    }
  done
  for family in ${_rhv_spec[role_families]}; do
    [[ -v _rhv_spec[secret_name:$family] ]] || {
      echo "render_host_values: spec declares family $family with no secret_name" >&2
      return 1
    }
  done

  host_base_anchor_map=$(printf '%s\n' \
    "tag: ${_rhv_spec[template_tag]}=tag: ${_rhv_spec[host_tag]}" \
    "namespace: ${_rhv_spec[template_namespace]}=namespace: ${_rhv_spec[namespace]}" \
    "replicas: ${_rhv_spec[template_replicas]}=replicas: ${render_replicas}")
  host_base=$out_dir/host-base.yaml
  awk -v anchor_map="$host_base_anchor_map" '
    BEGIN {
      rows = split(anchor_map, row, "\n")
      for (i = 1; i <= rows; i++) {
        if (row[i] == "") continue
        cut = index(row[i], "=")
        replacement[substr(row[i], 1, cut - 1)] = substr(row[i], cut + 1)
      }
    }
    {
      # Match the line CONTENT and re-emit at the file own indentation. The
      # anchors used to carry the template exact leading spaces, which coupled
      # the harness to a file it does not own: re-indenting the template made
      # every match miss at once. Content is what the anchor means; the indent
      # belongs to the template.
      body = $0
      sub(/^[ \t]+/, "", body)
      if (body in replacement) {
        indent = substr($0, 1, length($0) - length(body))
        print indent replacement[body]
        fired[body]++
        next
      }
    }
    { print }
    END {
      # EXACTLY once, not at least once. Matching on content rather than on the
      # full indented line widens what an anchor can reach, so the count is what
      # keeps it honest: a never-fired anchor is a drifted template, and a
      # twice-fired one is an anchor that has started matching a second place in
      # the tree. Both are silent corruption of the rendered values otherwise.
      for (anchor in replacement) {
        if (fired[anchor] == 1) continue
        printf "values overlay anchor matched %d times, want 1: [%s]\n", \
          fired[anchor], anchor > "/dev/stderr"
        wrong = 1
      }
      if (wrong) exit 1
    }
  ' "${_rhv_spec[host_base_template]}" >"$host_base"
  # Every secret anchor DERIVED, never spelled out. The generating rule is
  # workload_secret_name: wamn-<family>-<org>--<project>--<env>. A second
  # application declares its own families and identity and gets its own anchors;
  # nothing here reads as Receiving. The App family is separate because its
  # anchor is the platform-fixed wamn-host-db, not a derived name.
  host_overlay_anchor_map=$(
    printf '%s\n' \
      "namespace: ${_rhv_spec[template_namespace]}=namespace: ${_rhv_spec[namespace]}" \
      "replicas: ${_rhv_spec[template_replicas]}=replicas: ${render_replicas}" \
      "name: ${_rhv_spec[guest_secret_anchor]}=name: ${_rhv_spec[guest_secret_name]}"
    for family in ${_rhv_spec[role_families]}; do
      printf '%s\n' "name: wamn-${family}-${_rhv_spec[org]}--${_rhv_spec[project]}--${_rhv_spec[environment]}=name: ${_rhv_spec[secret_name:$family]}"
    done
  )
  host_overlay=$out_dir/host-overlay.yaml
  awk -v anchor_map="$host_overlay_anchor_map" \
      -v component_base="${_rhv_spec[component_artifact_base]}" \
      -v release_base="${_rhv_spec[release_artifact_base]}" -v digest="${_rhv_spec[manifest_digest]}" \
      -v nats_url="${_rhv_spec[nats_url]}" '
    BEGIN {
      rows = split(anchor_map, row, "\n")
      for (i = 1; i <= rows; i++) {
        if (row[i] == "") continue
        cut = index(row[i], "=")
        replacement[substr(row[i], 1, cut - 1)] = substr(row[i], cut + 1)
      }
    }
    {
      # Match the line CONTENT and re-emit at the file own indentation. The
      # anchors used to carry the template exact leading spaces, which coupled
      # the harness to a file it does not own: re-indenting the template made
      # every match miss at once. Content is what the anchor means; the indent
      # belongs to the template.
      body = $0
      sub(/^[ \t]+/, "", body)
      if (body in replacement) {
        indent = substr($0, 1, length($0) - length(body))
        print indent replacement[body]
        fired[body]++
        next
      }
    }
    /name: WAMN_WASMTIME_CACHE_DIR/ {
      print
      print "        - { name: OTEL_EXPORTER_OTLP_ENDPOINT, value: http://otel-collector.wamn-system.svc.cluster.local:4317 }"
      print "        - { name: OTEL_BSP_SCHEDULE_DELAY, value: \"1\" }"
      print "        - { name: OTEL_BSP_MAX_EXPORT_BATCH_SIZE, value: \"1\" }"
      next
    }
    /^        - name: WAMN_EVT_NATS_URL$/ {
      print
      getline
      print "          value: " nats_url
      next
    }
    /^          value: registry\.wamn-system\.svc\.cluster\.local:5000\/wamn\/components$/ {
      print "          value: " component_base; next
    }
    /^        - "--release-artifact-base=/ {
      print "        - \"--release-artifact-base=" release_base "\""; next
    }
    /^        - "--release-manifest-digest=/ {
      print "        - \"--release-manifest-digest=" digest "\""
      print "        - \"--allow-insecure-registries\""
      next
    }
    { print }
    END {
      # EXACTLY once, not at least once. Matching on content rather than on the
      # full indented line widens what an anchor can reach, so the count is what
      # keeps it honest: a never-fired anchor is a drifted template, and a
      # twice-fired one is an anchor that has started matching a second place in
      # the tree. Both are silent corruption of the rendered values otherwise.
      for (anchor in replacement) {
        if (fired[anchor] == 1) continue
        printf "values overlay anchor matched %d times, want 1: [%s]\n", \
          fired[anchor], anchor > "/dev/stderr"
        wrong = 1
      }
      if (wrong) exit 1
    }
  ' "${_rhv_spec[host_values_overlay]}" >"$host_overlay"

}
