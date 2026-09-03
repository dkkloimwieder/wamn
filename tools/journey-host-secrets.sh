#!/usr/bin/env bash
# Journey host-credential derivation, shared by every application journey.
#
#   derive_host_secrets <spec-array-name> <out-array-name>
#
# LAW: this function reads NOTHING but the array it is handed, writes NOTHING
# but the array it is told to write, and touches NO cluster. Applying the
# Secrets it validates is the journey's business, not this function's -- the
# two were one loop until the lift checklist separated them, and a block that
# mixes a pure derivation with an effect cannot be proven offline.
#
# Required spec keys:
#   secret_directory     where the journey's provisioning wrote its Secrets
#   role_families        space-separated; the families THIS application mounts
#   guest_secret_file    the App family's file, whose stem differs from every
#                        other family's because cli_stem and component_stem
#                        disagree for App and only for App
#   namespace            the namespace every Secret must declare
#   database_host        the host every Secret's url must point at
#
# Writes into the out array:
#   path:guest, name:guest          the App family's file and metadata.name
#   path:<family>, name:<family>    one pair per declared family
#
# Both parameters are namerefs, and both names are deliberately ugly. A nameref
# that shares a name with the caller's own variable resolves to the wrong one
# and bash only WARNS. The OUT nameref is the more dangerous of the two: a
# mis-bound write lands in another variable and the caller reads stale or empty
# data with nothing on stderr at the point of the read.

derive_host_secrets() {
  local -n _dhs_spec=$1
  local -n _dhs_out=$2
  local key family

  local required=(secret_directory role_families guest_secret_file namespace database_host)
  for key in "${required[@]}"; do
    [[ -v _dhs_spec[$key] ]] || {
      echo "derive_host_secrets: spec is missing $key" >&2
      return 1
    }
  done
  [[ -n ${_dhs_spec[role_families]} ]] || {
    echo "derive_host_secrets: spec declares no role families" >&2
    return 1
  }

  local directory=${_dhs_spec[secret_directory]}
  local guest_path=$directory/${_dhs_spec[guest_secret_file]}

  # The declared set IS the assertion. The count is whatever the application
  # named, so this reads the declaration rather than restating it -- a second
  # application changes the expectation by declaring, not by editing here.
  local -a emitted expected
  mapfile -t emitted < <(find "$directory" -maxdepth 1 -type f \
    -name '*.json' -printf '%f\n' | sort)
  mapfile -t expected < <(printf '%s\n' "${_dhs_spec[guest_secret_file]}" \
    "${_dhs_spec[role_families]// /.json$'\n'}.json" | sort)
  if [[ "${emitted[*]}" != "${expected[*]}" ]]; then
    echo "journey emitted ${#emitted[@]} host credential Secrets, not the ${#expected[@]} this application declares" >&2
    echo "  emitted:  ${emitted[*]}" >&2
    echo "  declared: ${expected[*]}" >&2
    return 1
  fi

  # Each Secret's shape, checked before anything is told about it. A Secret in
  # the wrong namespace or pointing at another database would otherwise be
  # mounted by a host that then fails far from here.
  #
  # Existence needs no check of its own: the declared set was compared against
  # the directory listing above, so every path below is one the listing
  # produced. An -f test here was unreachable, which a mutation pass showed by
  # deleting it and changing nothing.
  local path
  for family in guest ${_dhs_spec[role_families]}; do
    if [[ $family == guest ]]; then path=$guest_path
    else path=$directory/$family.json
    fi
    jq -e --arg namespace "${_dhs_spec[namespace]}" \
       --arg host "${_dhs_spec[database_host]}" '
      .metadata.namespace == $namespace and
      (.metadata.name | type == "string" and length > 0) and
      (.stringData.url | type == "string" and contains("@" + $host + ":5432/"))
    ' >/dev/null "$path" || {
      echo "derive_host_secrets: $path is not a Secret for ${_dhs_spec[namespace]} at ${_dhs_spec[database_host]}" >&2
      return 1
    }
    _dhs_out[path:$family]=$path
    _dhs_out[name:$family]=$(jq -er '.metadata.name' "$path")
  done
}
