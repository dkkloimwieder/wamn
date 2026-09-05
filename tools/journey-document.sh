#!/usr/bin/env bash
# Journey input document, shared by every application journey.
#
#   write_journey_document <spec-array-name> <schema-file> <out-file>
#   amend_journey_document <schema-file> <document-file> <key> <json-value>
#
# LAW: reads only the array it is handed and the files it is told to read or
# write; touches no cluster; returns rather than exits.
#
# AN ENVIRONMENT VARIABLE CARRIES A PROCESS SETTING. DATA CROSSES A BOUNDARY AS
# A DECLARED, SCHEMA'D ARTIFACT. The journey used to hand its Rust producer
# thirteen environment variables, one name at a time, each spelled for
# whatever its author was thinking about -- until two PG18 URLs sat one
# segment apart in a flat namespace with nothing to say they were different
# things. This writes ONE document instead. The Rust side reads it with a
# strict struct (deny_unknown_fields); this side checks the same contract
# BEFORE the Rust side ever runs, by reading the checked-in schema that struct
# generated.
#
# THE SCHEMA IS THE CONTRACT ON BOTH SIDES. The writer does not know the field
# list; it reads `required` and `properties` from the schema file the Rust
# struct generated and drift-tests. A key the spec supplies that the schema
# does not know is refused HERE, naming it, rather than by the Rust parser
# forty minutes into a cluster run. A required key the spec omits is refused
# the same way. So the shell and Rust cannot disagree on the field set without
# one of them failing on this machine.
#
# EVERY VALUE IS A STRING. That is the document's shape today -- URLs, paths,
# a namespace -- and jq's $ARGS.named builds exactly that object from --arg
# pairs, byte-stably under -S. A phase that becomes known later (the
# materializer's project database and receipt) is amended in as ONE object by
# amend_journey_document, so the document is written once and grown once,
# never rewritten.

write_journey_document() {
  local -n _wjd_spec=$1
  local schema=$2 out=$3
  local key
  local -a args=()

  [[ -f $schema ]] || {
    echo "write_journey_document: no schema at $schema" >&2
    return 1
  }
  jq -e '.required and .properties' "$schema" >/dev/null 2>&1 || {
    echo "write_journey_document: $schema does not carry required and properties" >&2
    return 1
  }

  # Every key the schema requires must be supplied, and named when it is not.
  while IFS= read -r key; do
    [[ -v _wjd_spec[$key] ]] || {
      echo "write_journey_document: spec is missing required $key" >&2
      return 1
    }
  done < <(jq -r '.required[]' "$schema")

  # Every key supplied must be one the schema knows -- the shell-side half of
  # deny_unknown_fields -- and must carry a value. Emitted in the schema's own
  # order so the bytes do not depend on bash's hash order.
  for key in "${!_wjd_spec[@]}"; do
    jq -e --arg key "$key" '.properties | has($key)' "$schema" >/dev/null || {
      echo "write_journey_document: spec carries $key, which the schema does not declare" >&2
      return 1
    }
    [[ -n ${_wjd_spec[$key]} ]] || {
      echo "write_journey_document: spec value for $key is empty" >&2
      return 1
    }
  done
  while IFS= read -r key; do
    [[ -v _wjd_spec[$key] ]] || continue
    args+=(--arg "$key" "${_wjd_spec[$key]}")
  done < <(jq -r '.properties | keys[]' "$schema")

  jq -nS '$ARGS.named' "${args[@]}" >"$out"
}

# Set ONE key of an existing document to a JSON value, once. The key must be
# one the schema declares and must not already be present: an amendment is a
# phase becoming known, not an edit, and setting it twice means the journey
# has lost track of which phase it is in.
amend_journey_document() {
  local schema=$1 document=$2 key=$3 value=$4
  local amended

  [[ -f $schema ]] || {
    echo "amend_journey_document: no schema at $schema" >&2
    return 1
  }
  [[ -f $document ]] || {
    echo "amend_journey_document: no document at $document" >&2
    return 1
  }
  jq -e --arg key "$key" '.properties | has($key)' "$schema" >/dev/null || {
    echo "amend_journey_document: $key is not a key the schema declares" >&2
    return 1
  }
  if jq -e --arg key "$key" 'has($key)' "$document" >/dev/null; then
    echo "amend_journey_document: $document already carries $key" >&2
    return 1
  fi
  jq -e . >/dev/null <<<"$value" 2>/dev/null || {
    echo "amend_journey_document: value for $key is not JSON" >&2
    return 1
  }

  amended=$(jq -S --arg key "$key" --argjson value "$value" '.[$key] = $value' "$document") || return 1
  printf '%s\n' "$amended" >"$document"
}
