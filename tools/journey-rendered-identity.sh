#!/usr/bin/env bash
# Journey rendered-identity assertion, shared by every application journey.
#
#   assert_rendered_identity <rendered-file> <claims-array-name>
#
# The claims array maps an env var name to the value THIS application declares:
#   declare -A claims=([WAMN_ORG]=acme [WAMN_PROJECT]=receiving [WAMN_SCHEMA]=receiving)
#
# WHY ASSERT RATHER THAN SUBSTITUTE. These values are what the overlay template
# is FOR -- the host derives its environment-scoped service subject from them,
# so they are the template's own claim of identity, not a slot for the harness
# to fill. A harness that rewrote them would be the credential-mounting hazard
# with better manners. What the harness owes is a check that the template it
# was pointed at is the one this application actually declares: a file copied
# from another application keeps that application's identity, the host mounts
# THIS journey's credentials under it, route authorization resolves against
# the wrong tenant, and the journey goes green.
#
# LAW: reads only its two arguments, writes nothing, touches no cluster, and
# RETURNS rather than exits -- a shared function that exits removes its caller's
# ability to add context or clean up.
#
# Three properties, each with its own message, because a refusal that could
# have come from any of them tells the reader nothing:
#   absent   -- the claim is not in the file at all (fail closed; a template in
#               an unexpected YAML form reports THAT, not a mismatch)
#   repeated -- the claim appears more than once (the file may declare one
#               identity twice, or two different ones, and the second is worse)
#   wrong    -- the claim is present once and disagrees with the declaration

assert_rendered_identity() {
  local rendered=$1
  local -n _ari_claims=$2
  local key found claimed

  [[ -f $rendered ]] || {
    echo "assert_rendered_identity: no rendered file at $rendered" >&2
    return 1
  }
  [[ ${#_ari_claims[@]} -gt 0 ]] || {
    echo "assert_rendered_identity: no claims declared to check" >&2
    return 1
  }

  for key in "${!_ari_claims[@]}"; do
    mapfile -t found < <(awk -v key="$key" '
      {
        body = $0
        sub(/^[ \t]+/, "", body)
        if (body !~ ("^- \\{ name: " key ",")) next
        if (!match(body, /value: [^ },]+/)) next
        print substr(body, RSTART + 7, RLENGTH - 7)
      }
    ' "$rendered")

    if [[ ${#found[@]} -eq 0 ]]; then
      echo "rendered file declares no $key" >&2
      return 1
    fi
    if [[ ${#found[@]} -gt 1 ]]; then
      echo "rendered file declares $key ${#found[@]} times: ${found[*]}" >&2
      return 1
    fi
    claimed=${found[0]}
    if [[ $claimed != "${_ari_claims[$key]}" ]]; then
      echo "rendered file claims $key=$claimed, but this journey declares ${_ari_claims[$key]}" >&2
      return 1
    fi
  done
}
