#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.5.1"
readonly OUTCOME="HTTP begin admits one ordinary unleased queue row and never enters a guest"
readonly CAMPAIGN="http-ordinary-admission"
readonly BEAD="wamn-0h0g.5.1"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
command -v perl >/dev/null || {
  echo "perl is required for byte-exact mutation replacement" >&2
  exit 2
}
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    http-queue-preclaim-restored \
    begin-starts-inline-driver
}

load_mutation() {
  local id="$1"
  case "$id" in
    http-queue-preclaim-restored)
      TARGET="crates/execution/run-state/src/admission.rs"
      EXPECTED_SHA="4537be786450218be3403e625b4c7c0afe36ed4bc7c532f4f6ea36966e1b9712"
      NEEDLE=$'      (tenant_id, run_id, partition_key, partition_policy, available_at, stream_seq) \\\n    SELECT r.tenant_id, r.run_id, c.partition_key, c.partition_policy, now(), \\\n           CASE WHEN c.producer = \'event\' THEN c.event_seq ELSE 0 END \\'
      REPLACEMENT=$'      (tenant_id, run_id, partition_key, partition_policy, available_at, \\\n       lease_owner, lease_expires_at, lease_generation, stream_seq) \\\n    SELECT r.tenant_id, r.run_id, c.partition_key, c.partition_policy, now(), \\\n           CASE WHEN c.producer = \'http\' THEN \'inline-mutant\' END, \\\n           CASE WHEN c.producer = \'http\' THEN now() + interval \'30 seconds\' END, \\\n           CASE WHEN c.producer = \'http\' THEN 1 ELSE 0 END, \\\n           CASE WHEN c.producer = \'event\' THEN c.event_seq ELSE 0 END \\'
      GATE="admission::tests::producer_specific_checks_and_unleased_queue_state_are_pinned"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    begin-starts-inline-driver)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="7bab1ff9c3f961c5028f6502ae32e7145329f84dafde900a05bfb0ccd4f92c66"
      NEEDLE=$'                AdmissionResult::Admitted { run_id } => {\n                    return Ok(BeginResult::Admitted(Admitted { run_id }));\n                }'
      REPLACEMENT=$'                AdmissionResult::Admitted { run_id } => {\n                    self._driver.start(InlineRunClaim {\n                        run_id: run_id.clone(),\n                        lease_owner: self.config.executor_id.clone(),\n                        lease_generation: 1,\n                        tenant: self.config.tenant_id.clone(),\n                        project: self.config.project.clone(),\n                        schema: self.config.schema.clone(),\n                    })?;\n                    return Ok(BeginResult::Admitted(Admitted { run_id }));\n                }'
      GATE="flow_invocation::tests::own_plan_requirement_allows_only_pure_call_free_requests_without_a_key"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --lib "$GATE" -- --exact)
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
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" perl -0ne '
    BEGIN { $needle = $ENV{NEEDLE}; $count = 0 }
    $count += () = /\Q$needle\E/g;
    END { print $count }
  ' "$TARGET")"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once; found $count" >&2
    return 2
  fi
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e '
    BEGIN {
      $needle = $ENV{NEEDLE};
      $replacement = $ENV{REPLACEMENT};
      $count = 0;
    }
    $count += s/\Q$needle\E/$replacement/;
    END { exit($count == 1 ? 0 : 1) }
  ' "$TARGET"
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
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
  run_gate
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
