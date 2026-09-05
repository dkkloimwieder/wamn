#!/usr/bin/env bash
# Journey mint helpers, shared by every application journey that mints its
# release from the shell through wamn-ctl's verbs.
#
#   render_component_declaration <template.in> <out.json> <tenant> <package-id> <package-version> <store-alias>
#   gate_document <package-id> <package-version> <project> <environment> <wiring-id> <wiring.json>
#   family_generation_flags <out-array-name> <secret-directory> <guest-secret-file> <family>...
#
# LAW: each reads only what it is handed and the files it is told to read or
# write; touches no cluster and no database; returns rather than exits.
#
# WHY THESE THREE AND NOT THE MINT ITSELF. The mint is a sequence of verbs
# against a live registry and database -- an effect, proven only by a cluster
# run. These are the pure text it needs on the way: a declaration with its
# placeholders filled, the envelope the authoring gate accepts, and the flag
# set that asks provisioning for one credential per declared family. Each can
# be rendered with pinned inputs and compared, so a wrong placeholder or a
# misspelled flag fails here rather than forty minutes into a run.

# Fill a component declaration template. Every placeholder the template
# carries must be filled; one surviving is refused BY NAME, because a
# declaration pushed with __TENANT_ID__ in it is admitted under a tenant
# literally called that.
render_component_declaration() {
  local template=$1 out=$2 tenant=$3 package_id=$4 package_version=$5 store_alias=$6
  local survivor
  [[ -f $template ]] || {
    echo "render_component_declaration: no template at $template" >&2
    return 1
  }
  for value in "$tenant" "$package_id" "$package_version"; do
    [[ -n $value ]] || {
      echo "render_component_declaration: tenant, package id and version must all be given" >&2
      return 1
    }
  done
  sed -e "s/__TENANT_ID__/$tenant/g" -e "s/__PACKAGE_ID__/$package_id/g" \
      -e "s/__PACKAGE_VERSION__/$package_version/g" -e "s/__STORE_ALIAS__/$store_alias/g" \
      "$template" >"$out" || return 1
  survivor=$(grep -oE '__[A-Z_]+__' "$out" | head -1 || true)
  [[ -z $survivor ]] || {
    echo "render_component_declaration: $out still carries the placeholder $survivor" >&2
    return 1
  }
  jq -e '.scope["tenant-id"] and .scope["package-id"] and .scope["package-version"] and .component' \
    >/dev/null "$out" 2>/dev/null || {
    echo "render_component_declaration: $out is not a declaration with a scope and a component" >&2
    return 1
  }
}

# The authoring gate's request envelope for one wiring, on stdout. Shape
# copied from the Rust producer's gate_document: a request document carrying
# one gate command whose input scopes the wiring to a project environment and
# a package coordinate. The wiring document is embedded whole, parsed, so a
# file that is not JSON fails here and not as a 400 from the gate.
gate_document() {
  local package_id=$1 package_version=$2 project=$3 environment=$4 wiring_id=$5 document=$6
  [[ -f $document ]] || {
    echo "gate_document: no wiring document at $document" >&2
    return 1
  }
  jq -n --arg id "gate-$package_id-$wiring_id" --arg project "$project" --arg env "$environment" \
    --arg package "$package_id" --arg version "$package_version" --slurpfile document "$document" '
    if ($document | length) != 1 then error("wiring document must be exactly one JSON value") else empty end,
    {
      document: "request",
      body: {
        "schema-version": "0.1",
        "command-id": $id,
        command: {
          kind: "gate",
          input: {
            scope: {"project-id": $project, environment: $env},
            "package-id": $package,
            "package-version": $version,
            document: $document[0]
          }
        }
      }
    }' 2>/dev/null || {
    echo "gate_document: $document is not one JSON document" >&2
    return 1
  }
}

# The provisioning flags that ask for one prepared credential per declared
# family, written where derive_host_secrets will look for it: <family>.json
# for each family and the declared guest file for the App family, whose
# cli_stem is "guest". Appends to the named array.
family_generation_flags() {
  local -n _fgf_out=$1
  local directory=$2 guest_file=$3
  shift 3
  local family
  [[ -n $directory && -n $guest_file ]] || {
    echo "family_generation_flags: secret directory and guest file must be given" >&2
    return 1
  }
  [[ $# -gt 0 ]] || {
    echo "family_generation_flags: no families declared" >&2
    return 1
  }
  for family in "$@"; do
    [[ $family =~ ^[a-z]+(-[a-z]+)*$ ]] || {
      echo "family_generation_flags: $family is not a family stem" >&2
      return 1
    }
    _fgf_out+=(--prepare-"$family"-generation a --emit-"$family"-secret "$directory/$family.json")
  done
  _fgf_out+=(--prepare-guest-generation a --emit-guest-secret "$directory/$guest_file")
}
