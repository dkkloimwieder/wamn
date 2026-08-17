#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.5.13"
readonly OUTCOME="plan supply refuses untrusted bytes and never crosses tenant boundaries"
readonly CAMPAIGN="plan-supply"
readonly BEAD="wamn-0h0g.5.13"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT EXPECTED_COUNT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' hash-verification-bypass cache-drops-tenant
}

load_mutation() {
  local id="$1"
  case "$id" in
    hash-verification-bypass)
      TARGET="crates/platform/runtime/src/plugins/runner_plan_supply.rs"
      EXPECTED_SHA="735fb0613ad0ceb5602d2cd0a03c34cd6b7d2dec4e0364446832a61e705ab9b7"
      NEEDLE='if execution_bundle_hash_of(&exact_bytes) != execution_bundle_hash {'
      REPLACEMENT='if false {'
      EXPECTED_COUNT=1
      GATE="plugins::runner_plan_supply::tests::hash_mismatch_never_enters_the_cache"
      ;;
    cache-drops-tenant)
      TARGET="crates/platform/runtime/src/plugins/runner_plan_supply.rs"
      EXPECTED_SHA="735fb0613ad0ceb5602d2cd0a03c34cd6b7d2dec4e0364446832a61e705ab9b7"
      NEEDLE=$'        let key = PlanCacheKey {\n            tenant_id: Arc::from(tenant_id),\n            execution_bundle_hash: Arc::from(execution_bundle_hash),\n        };\n        let bytes = state.entries.get(&key)?.clone();'
      REPLACEMENT=$'        let key = PlanCacheKey {\n            tenant_id: Arc::from(""),\n            execution_bundle_hash: Arc::from(execution_bundle_hash),\n        };\n        let bytes = state.entries.get(&key)?.clone();'
      EXPECTED_COUNT=1
      GATE="plugins::runner_plan_supply::tests::cache_is_entry_bounded_and_tenant_scoped"
      ;;
    # `moving-head-query` is removed, not repaired (wamn-0h0g.15.12). It anchored
    # on `JOIN run_flow_resolutions AS resolution` in the plan-bytes statement,
    # and asserted that plan supply read the run's immutable resolution map rather
    # than a moving catalog head. Both halves are gone: wamn-0h0g.15.10 deleted the
    # table, and this bead deleted the statement — plan bytes no longer come from
    # the database at all, so there is no query left for a moving head to leak
    # into. Its gate was renamed for the same reason (see
    # `run_release_binding_reads_one_tenant_scoped_run_row`).
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
  TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == "$EXPECTED_COUNT" ]] || {
    echo "$TARGET must contain mutation anchor $EXPECTED_COUNT time(s) (found $count)" >&2
    return 2
  }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
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
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  }
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  }
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
  list) mutation_ids ;;
  check) check_campaign ;;
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
  *) usage; exit 2 ;;
esac
