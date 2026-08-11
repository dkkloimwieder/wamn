#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="admission-execution-pin"
readonly BEAD="wamn-0h0g.2.4"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name an isolated debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    release-member-hash-replaced-by-caller-json \
    missing-root-plan-admits-run \
    runs-execution-bundle-fk-removed \
    run-admission-pin-update-allowed \
    draft-json-bundle-pin-restored \
    legacy-plan-pin-fabricated-from-artifact
}

load_mutation() {
  local id="$1"
  case "$id" in
    release-member-hash-replaced-by-caller-json)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="b003f77129de11e84419341895eba94ccda40a244c0a887f3d3a94c6df9fefc8"
      NEEDLE="                  member.execution_bundle_hash, '{dispatched}', 'scenario', \\"
      REPLACEMENT="                  \$7::text::jsonb ->> 'execution-bundle-hash', '{dispatched}', 'scenario', \\"
      GATE="admission_derives_root_bundle_from_authoritative_member"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    missing-root-plan-admits-run)
      TARGET="crates/execution/run-state/src/admission.rs"
      EXPECTED_SHA="145b1e7656f46ed60f998adc19c8b262e5fa51dde221cd229a2e3344df0c6575"
      NEEDLE="      WHEN rp.execution_bundle_hash IS NULL THEN 'missing-root-plan' \\"
      REPLACEMENT="      WHEN rp.execution_bundle_hash IS NULL THEN 'ready' \\"
      GATE="admission::tests::admission_derives_root_bundle_from_authoritative_member"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    runs-execution-bundle-fk-removed)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5d8a8967ba040f6f55a8277b2ad01a21815b0fecbc3c94c633bd0d69ca9832b5"
      NEEDLE='    CONSTRAINT runs_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)'
      REPLACEMENT='    CONSTRAINT runs_execution_bundle_present
        CHECK (execution_bundle_hash IS NOT NULL)'
      GATE="run_plane::tests::execution_pin_schema_of_record_is_exact_and_complete"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    run-admission-pin-update-allowed)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5d8a8967ba040f6f55a8277b2ad01a21815b0fecbc3c94c633bd0d69ca9832b5"
      NEEDLE='CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash'
      REPLACEMENT='CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF catalog_id, catalog_version, environment'
      GATE="run_plane::tests::execution_pin_schema_of_record_is_exact_and_complete"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    draft-json-bundle-pin-restored)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="b003f77129de11e84419341895eba94ccda40a244c0a887f3d3a94c6df9fefc8"
      NEEDLE="                      'validated-draft-hash', d.validated_draft_hash, \\"
      REPLACEMENT="                      'validated-draft-hash', d.validated_draft_hash, \\
                      'execution-bundle-hash', d.execution_bundle_hash, \\"
      GATE="draft_admission_persists_validated_bundle_without_json_duplicate"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test draft_admission "$GATE" -- --exact)
      ;;
    legacy-plan-pin-fabricated-from-artifact)
      TARGET="services/ctl/src/publish_catalog.rs"
      EXPECTED_SHA="08e3581c29d9b91c52196df4d469e0c69387395d8c1f17d65b7cdeb0b3ce1f64"
      NEEDLE='        artifact,
        supplied_node_types,
        execution_bundle_hash: None,'
      REPLACEMENT='        execution_bundle_hash: Some(
            artifact.identity().artifact_hash().as_str().to_string(),
        ),
        artifact,
        supplied_node_types,'
      GATE="publish_catalog::tests::legacy_publication_without_validated_plan_returns_missing_root_plan"
      TEST_ARGV=(cargo test --locked -p wamn-ctl --lib "$GATE" -- --exact)
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  fi
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; path=pathlib.Path(os.environ["TARGET"]); data=path.read_text(); path.write_text(data.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
  assert_precondition

  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
  }
  trap restore EXIT INT TERM

  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  if [[ "$mutant_sha" == "$EXPECTED_SHA" ]]; then
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  fi

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
  exit_code=$?
  set -e
  if [[ $exit_code -eq 0 ]]; then
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  fi
  echo "KILLED id=$id gate=$GATE exit_code=$exit_code"
)

check_campaign() {
  local id
  while IFS= read -r id; do
    load_mutation "$id"
    assert_precondition
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' \
      "$id" "$GATE" "$TARGET" "$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() {
  echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2
}

case "${1:-}" in
  list)
    mutation_ids
    ;;
  check)
    check_campaign
    ;;
  green)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_green "$2"
    ;;
  green-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_green "$id"; done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids)
    ;;
  *)
    usage
    exit 2
    ;;
esac
