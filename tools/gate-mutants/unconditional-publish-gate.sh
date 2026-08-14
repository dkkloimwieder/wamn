#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="unconditional-publish-gate"
readonly BEAD="wamn-0h0g.8.8"
readonly EXPECTED_PROFILE="debug"
readonly TARGET="crates/authoring/model/src/lib.rs"
readonly EXPECTED_SHA="e061bff2ad1a49e1e23df8e643c6b9aff1451af5c1bcf6c3ddff70639c6c8722"
readonly NEEDLE='    pub successful_report_id: String,'
readonly REPLACEMENT='    #[serde(default)]
    pub successful_report_id: String,'
readonly GATE="publish_requires_a_successful_report_unconditionally"
readonly -a TEST_ARGV=(cargo test --locked --offline -p wamn-authoring-model \
  --test contract "$GATE" -- --exact)

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the isolated debug target directory" >&2
  exit 2
fi

sha256() {
  sha256sum "$TARGET" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(MUTATION_NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{MUTATION_NEEDLE}\E/g; END { print $count }' "$TARGET")"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  }
}

green() {
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
}

run_mutant() (
  local backup_dir backup restored_sha mutant_sha exit_code
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256)"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
    echo "RESTORED campaign=$CAMPAIGN target=$TARGET sha256=$restored_sha"
  }
  trap restore EXIT INT TERM

  MUTATION_NEEDLE="$NEEDLE" MUTATION_REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{MUTATION_NEEDLE}\E/$ENV{MUTATION_REPLACEMENT}/' "$TARGET"
  mutant_sha="$(sha256)"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation did not change $TARGET" >&2
    exit 3
  }

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED gate=$GATE" >&2
    exit 1
  }
  echo "KILLED gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check)
    assert_precondition
    echo "CHECKED campaign=$CAMPAIGN target=$TARGET sha256=$EXPECTED_SHA"
    ;;
  green)
    green
    ;;
  run)
    run_mutant
    ;;
  *)
    echo "usage: $0 check | green | run" >&2
    exit 2
    ;;
esac
